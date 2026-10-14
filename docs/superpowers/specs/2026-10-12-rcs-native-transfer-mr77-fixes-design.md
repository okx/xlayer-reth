# MR77 RCS Filter — Nested CREATE Endowment Fix + Top-level native Transfer Unification — Design

> **CURRENT APPROVED DESIGN — revision r11 (2026-10-13).** This file is the authoritative in-repo mirror of the
> approved design. The canonical publication is the Lark Doc
> https://okg-block.sg.larksuite.com/docx/UXohdEdeaokIGpxWnJPlaqGugac (doc token `UXohdEdeaokIGpxWnJPlaqGugac`),
> which carries the COMPLETE current Spec. The Lark publication record is `docs/superpowers/lark-review-doc.md`.

## Approval (read first)

| Field | Value |
|---|---|
| Revision | **r11** (2026-10-13), supersedes r10 |
| Decision | **PASS** — `PASS：设计没问题，继续` |
| approval_mode | `delegated_agent` (parent building Agent review; **no human signature**) |
| Reviewer | `starport-rcs` (BUILDING_ID, via Terran Bus) |
| Unit | `viking-nativetransfer-starport-rcs` (UNIT_ID) |
| Request message_id | `viking-nativetransfer-starport-rcs-design-review-r11-20261013` |
| Reply message_id | `starport-rcs-design-review-r11-pass-20261013` |
| Reply Bus record_id | `recvxVksp5gum4` |
| correlation_id | `player-fix-mr77-native-transfer-2items-20261012` |
| Design URL | https://okg-block.sg.larksuite.com/docx/UXohdEdeaokIGpxWnJPlaqGugac |
| Canonical spec content hash (sha256, r11) | `49dce95ee1dd5f4f37fcabd0ae24c084447a10b111cc7acaea20f8b141970836` |
| Lark last_modified at PASS | 2026-10-13T10:15:56Z |
| Plan baseline | `a0e5d7b73a669d50ed18f65d1c40c52a7a70a230` (MR77 round-4 head) — **NOT** 21a311e9 |
| MR (reused) | https://gitlab.okg.com/github/xlayer-reth/-/merge_requests/77 |
| Jira | XLOP-1228 (Task) under Epic XLOP-1217 |
| Scope of this PASS | **Design only.** Does NOT authorize merge, deploy, release, MR-approve, cleanup/finalization, or scope expansion. |

Reviewer-issued execution correction carried with the PASS (no new design cycle required): the node manifest package is
`xlayer-reth-node` (`bin/node/Cargo.toml`), **not** `xlayer-node`. Use
`cargo tree -p xlayer-reth-node -e normal,build,features` for the default-production feature-graph evidence
(default feature set, no `--all-features`, dev edges excluded).

## Integrity and provenance of this file

- `2026-10-12-rcs-native-transfer-mr77-fixes-design-r11-canonical-body.txt` in this directory is the **byte-identical
  approved r11 body**; its sha256 is `49dce95ee1dd5f4f37fcabd0ae24c084447a10b111cc7acaea20f8b141970836`, which is
  exactly the canonical content hash named in the reviewer's PASS reply. Verify with
  `sha256sum docs/superpowers/specs/2026-10-12-rcs-native-transfer-mr77-fixes-design-r11-canonical-body.txt`.
- The complete approved body is reproduced verbatim in **§A** below, so this single file is self-sufficient for
  downstream stages.
- `history/2026-10-12-rcs-native-transfer-mr77-fixes-design-r4.md` is the superseded r4 structured markdown recovered
  from the stage-5 `superpowers-context.tar.gz` checkpoint. It is **history only** — do not implement from it.
- Known textual artifact: inside the approved body, the r11 header line quotes a self-referential hash
  (`a2ac22f4...`) that was computed before the final publication edit. The authoritative canonical hash for r11 is
  `49dce95e...`, which is the value the reviewer declared and retained in the PASS. Both ledgers agree on `49dce95e`.

## §A — Complete approved r11 design body (verbatim)

The text below is the verbatim plain-text body of the approved Lark Doc at revision r11, including all design
decisions D1–D9 / D7a / D-ISO / D7b, rationale, rejected alternatives, constraints, the acceptance/test contract
(r10 IDs plus the five r11 additions), Additional Context, and non-goals/anti-goals.

```text
文档纯文本内容如下：

MR77-RCS-native-transfer-fixes-design-r1-20261012
MR77 RCS Filter — Nested CREATE Endowment Fix + Top-level native Transfer Unification — Design (revision r10)
Delegated technical review target. Reviewer: building starport-rcs (via Terran Bus). This document is mirrored verbatim to the Lark review doc. The SAME doc, updated in place across r1→r2→r3→r4→r5→r6→r7→r8→r9→r10. The Lark publication record (full URL / token / revision / content hash) is in docs/superpowers/lark-review-doc.md.

Revision: r10 — 2026-10-13 (supersedes r9). Canonical spec content hash (sha256, r10): 87cb906d8da9422db3c5d75ecf4464cf84b5e20b62e5725419596c0e5d203c68
MR head (impl under revision): 21a311e93d624e6a252c16952680437c8d2c5525

r9 was design CHANGES_REQUESTED (starport-rcs-design-review-r9-changes-20261013, Bus recvxUH7IxcuCV) for three assertion-wording items. r10 strengthens exactly those assertions — D1–D9/D7a/D-ISO/D7b mechanism UNCHANGED. Plan baseline = 21a311e9 (unchanged). See §0.8, §8.r10, §13.r10 of canonical in-repo spec. Corrections A (test-utils feature) and B (complete Lark publication) were ACCEPTED in r9 and carry forward unchanged.
Approval_mode = delegated_agent; design PASS authorizes neither merge, deploy, release, MR-approve, nor scope expansion.
0. Provenance and labeling (read first)
This is a newly authored, clearly-labeled revised spec for the two Player-approved MR77 fixes. Its requirement basis is
the Player-selected document https://okg-block.sg.larksuite.com/docx/HVoWdmiGcoPL4hxMJwglbObAgRh ("MR77 rcs-filter
内部原生转账观测 — 待修复问题", last modified 2026-10-12). Per the Player's explicit decision
(player-starport-mr77-provided-doc-current-basis-20261012), that document is the current requirement input; this spec
does NOT claim to be, or to have recovered, the historical "R12" spec
(`docs/superpowers/specs/2026-09-28-rcs-native-transfer-observation-design.md`), which is NOT present on this branch at
this SHA (GitLab get_file_contents returns File-not-found at 714e1293). Where this spec restates
previously-approved MR77 behavior, the source is the MR77 description plus the verbatim on-disk source at 714e1293,
both cited inline. New decisions introduced by this revision are flagged **[NEW DECISION]**.
0.1 What changed across r1 → r2 → r3 (responses to the building's CHANGES_REQUESTED)
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
1. Intent and why
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
2. Background — the MR77 capture architecture (preserved, cited)
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
3. Problems — root causes (code-grounded)
Problem 1 — nested CREATE endowment missed (bug; NT2)
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
Problem 2 — top-level tx.value not unified (design)
Top-level tx.value is matched only via the tx-level `value` binding (from `ScreenInput.value`; matching.rs:241), while
internal transfers are matched via `transfer.*` event bindings decoded from virtual logs. Two surfaces ⇒ a rule cannot
cover top-level + internal with one `transfer.*` condition. The MR77 design (AC2) deliberately emitted no virtual log
for top-level tx.value; this revision changes that.
4. Resolved design decisions
**Core insight (r2):** both problems are resolved by the *same* minimal change — **stop special-casing the top-level
frame and stop classifying frames at all; observe the real native value of every frame through the single funnel.**
The top-level value is simply the root frame's own transfer; nested-create endowment is simply a non-top frame's
transfer. Removing the `depth`-based top-level exclusion fixes NT2 (nested CREATE is no longer dropped) and unifies the
top-level value (the root frame now emits a `transfer.*` log). No `caller`/`origin` identity test and no hook-time
`journal().depth()` are used for the decision.
D1 — Remove top-level/internal classification; observe every frame's real native value [NEW DECISION; replaces r1 §D1 `caller==origin`]
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
D2 — Top-level native transfer is the root frame's own observation [NEW DECISION; replaces r1 synthesis]
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
D3 — ScreenInput.value: RETAIN + deprecate (do not remove in this change) [confirmed retained by reviewer; claim re-scoped]
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
D4 — Top-level contract creation without tx.to [confirmed retained by reviewer]
For a top-level CREATE (deployment tx, `tx.to == None`) with non-zero value, the endowment recipient is the created
contract address, which the `create` hook already derives (nonce-derived for CREATE, salt/init-code for CREATE2;
rcs_capture.rs:414-425). Emit `Transfer(origin, created_address, tx.value)`. Do NOT substitute the zero address or null
(rejected: misleading `transfer.to`; §6 R-D). The tx-level `contract_address` binding remains null for creations
(unchanged). The address-derivation must be covered by a precise `CreateOutcome`/nonce-address test (§8 item 2).
D5 — Ordering
Hooks fire in execution order; the root frame's hook fires before any inner frame or log, so the top-level transfer is
the first entry of the ordered stream handed to `screen_tx`: `[top-level transfer, …real + internal-virtual logs in
execution order]`. The RealLog cross-check is unaffected (it ignores virtual logs).
D6 — Budget counting [confirmed uniform by reviewer; matcher-budget wording corrected]
The top-level transfer flows through `observe_native_transfer` and therefore counts against
`max_native_transfers_per_tx` like any other observation (uniform single counter; it is at most +1 and deterministic).

**Matcher-binding budget, stated precisely (reviewer item 2):** per `matching.rs:128-139` the complete-binding count
for a rule is the **product** of its per-event candidate counts (`checked_binding_count`), and these products are
**accumulated across all rules** into `complete_binding_count`, which fails closed (`Deny`) once it exceeds
`max_event_bindings_per_tx`. Adding the top-level transfer increases the candidate count of the `transfer`-shaped event
by one wherever such a log is now present; for a rule declaring the same transfer alias twice, its contribution grows
from `N²` to `(N+1)²`. The design does not change the matcher; it must be shown (§8 item 7) that realistic rule sets
stay within the default budget and that the `limit+1` boundary still denies. Both budgets keep default 10000 and `>= 1`.
D7 — Rollback / early-failure / parent-child, layered [reviewer item 3]
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
D7a — Creation completion validity: gate on `CreateOutcome.address`, not only `is_ok` [NEW DECISION r3; resolves r2 CHANGES]
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
D8 — No dedup; zero/self filtered (unchanged)
Identical `(from,to,value)` transfers are NOT deduplicated (AC3) — unchanged. Zero-amount and self-transfers never
qualify (funnel `value.is_zero() || from == to`) — unchanged; a top-level `tx.value == 0` or self-send emits no
top-level transfer.
D9 — Receipt/RPC/consensus unchanged (unchanged)
Virtual native Transfer logs (top-level and internal) are injected only into the screening input. They never reach
receipts, RPC logs, bloom, the indexer, or consensus/commit state. No change to this invariant.
5. Affected files / interfaces / callers (blast radius)
Grounded in the codegraph index at 714e1293 (relations) + verbatim reads (line numbers at 9867fd48).

[r6/r7 CURRENT blast radius] Production change spans TWO files — rcs_capture.rs AND context.rs — NOT rcs_capture.rs alone. The r3/r4 phrasing 'rcs_capture.rs the only production change site' / 'finish_capture unchanged' / 'context.rs no structural change' / 'net localized to rcs_capture.rs' is SUPERSEDED and is not the current impact surface.

- crates/builder/src/flashblocks/rcs_capture.rs — primary production change site (capture inspector):
  - create hook (:400-425 @9867fd48): value-only decision (depth gate removed); create_recipients push + created-address derivation are active-gated (D-ISO, after `if !self.active { return None; }`).
  - call hook: value-only observe when transfer_value() is a non-zero Some.
  - record_call_observation / record_create_observation: value-only decisions funnelling into observe_native_transfer.
  - create_end (:427-444 @9867fd48): (a) !is_ok() → truncate (D7); (b) is_ok && address.is_none() → revoke/truncate (D7a nonce-overflow phantom); (c) [D7b] is_ok && Some(a) with expected Some(e), e!=a → set a sticky CaptureInvariantError::CreateAddressMismatch (NOT silent pop_checkpoint(true)). observed_count stays sticky.
  - finish_capture (:289-355 @9867fd48) — CHANGED: in addition to the existing RealLog cross-check (whose semantics are unchanged) and LimitExceeded, it now surfaces the sticky CreateAddressMismatch as a non-Complete InvariantViolation, and the every-exit clear resets the new flag. It is NOT 'unchanged'.
  - The RcsCaptureControl seam and the RealLog invariant are preserved; rcs-filter gains no new runtime dependency.
- crates/builder/src/flashblocks/context.rs — ONE production change [D7b]: fail_closed_screen (:110-160) gains a new arm for InvariantViolation{ reason: CaptureInvariantError::CreateAddressMismatch{..} } → bounded error! + record_local_deny(ObservationInvariant) + Some(Screen::Deny). The exhaustive match + the :146-149 comment make this arm compiler-mandatory. The screening call site (:987-1000) is otherwise unchanged: screen_tx runs ONLY for Complete(logs); every non-Complete outcome (incl. CreateAddressMismatch) fails closed via fail_closed_screen WITHOUT calling screen_tx, entering the pending buffer, or committing. ScreenInput.value kept (D3). The test-35 seams (LAST_SCREEN_DECISION, FORCE_REALLOG_MISMATCH) remain intact and are reused by the D-MM guard.
- crates/rcs-filter/src/handle.rs, matching.rs — NO production change (D3 retains value; D7/T3f). screen_tx=handle.rs:273 (public), screen_existing=handle.rs:463 (private), build_bindings=matching.rs:230 unchanged. rcs-filter/Cargo.toml has no builder/OpEvm dependency, so the real-capture→screen_tx Item-1 tests live in crates/builder (§8.r6 Correction C r7), not here.

Net builder-side production change: rcs_capture.rs (gates/value-only + D7a address gating + D7b sticky invariant + finish_capture branch + the CreateAddressMismatch variant) AND context.rs (the one new fail_closed_screen arm), plus tests. No upstream-reth modification; extension stays via the inspector/trait seam.
6. Rejected / considered alternatives
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
7. Constraints
- Only the two approved changes + necessary tests/spec. Reuse MR77 and branch `xl/xlop-1217/0928-1135`; preserve others'
  edits and the `deps/optimism` submodule pointer. No merge/deploy/chain action, no extra repo scope, no new Jira.
- Must pass repository-required formatting/lint: `cargo fmt` (scoped per package, never `--all`) and
  `cargo clippy --all-targets --workspace -- -D warnings`.
- Retain the test-35 safety evidence and all existing passing controls; do not weaken fail-closed behavior.
- Virtual logs never enter receipt/RPC/bloom/indexer/consensus (D9).
8. Test plan (executed in the implementation stage, not this design stage)
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
9. Non-goals / anti-goals
- **Non-goals:** merging/deploying MR77; any chain/consensus/receipt/RPC change; RCS-service-side rule authoring or
  migration (AC7); removing `ScreenInput.value` now; new Jira issues or repo scope; touching the `deps/optimism`
  submodule.
- **Anti-goals** (failure even if it "works"): fixing NT2 by any mechanism that re-introduces a depth/timing dependency
  or a `caller`/`origin` identity test; emitting the top-level transfer twice, or producing two actions for one
  movement **within the same `audit_type`** (distinct `audit_type`s each carrying one action for the same log is normal
  matcher behavior — see D6/§13); retaining a virtual endowment for an `is_ok` CREATE early-return that created no
  contract (D7a); letting any virtual log leak into a receipt/consensus; weakening the test-35 fail-closed proof;
  claiming a classifier correct without real OpEvm hook tests (incl. 7702/DELEGATECALL).
10. Additional Context — AC1–AC8 mapping (preserved from MR77, with r2 deltas)
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
  7702/DELEGATECALL + layered-rollback + is_ok early-return (6a) tests (§8).
- **AC7** (RCS-service confirmation of special-address role, Transfer ABI/`transfer` alias, no-tokenId ActionItem,
  virtual non-receipt semantics) — remains a non-code precondition owned by the RCS side; `ScreenInput.value` retention
  (D3) keeps existing rules working during migration, with the re-scoped caveat that the `transfer.*` log set grows.
- **AC8** (clean baseline; submodule untouched; fmt/clippy/targeted tests green) — preserved for the new change.
11. Open items for the reviewer (building starport-rcs)
None blocking. For explicit confirmation during review:

- D1/D2 (remove classification; top-level value is the root frame's own observation) — confirm this is the preferred
  resolution of r1 item 1 over a frame-ancestry marker (R-B).
- D7a (gate creation validity on `CreateOutcome.address`, not only `is_ok`) — confirm this resolves the r2 early-return
  leak as intended.
- D3 (retain `ScreenInput.value` + deprecate; `transfer.*` log set grows) — confirm this matches the RCS-service
  migration expectation.
- D6 (top-level counts against `max_native_transfers_per_tx`; matcher binding grows per the product rule) — confirm the
  budget treatment is acceptable.
12. Builder-check (self-review)
Read back as a fresh builder: the spec gives one coherent mechanism (observe every frame's real native value, no
classification) that a competent Rust/revm engineer can implement without returning to the author; every new decision is
flagged, grounded in cited source, and paired with its rejected alternative; the test plan is falsifiable (RED-first
NT2; top-level, 7702/DELEGATECALL, and `is_ok` early-return tests prove the root-hook dependency and the D7a leak fix)
and preserves test 35. Residual implementation freedom (exact fixture construction, whether 7702 is active on this
chainspec) is intentionally left to the builder and marked as such.
13. CHANGES_REQUESTED — item-by-item responses (r1 / r2 / r3 / r4)
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
r4 revision — Stage-5.0 MR CHANGES_REQUESTED (five P2 items; grounded at fb43bf28)
0.2 Overview. r3 was design-approved; its implementation at fb43bf28 is directionally correct (depth-gate removal, CreateOutcome.address rollback, deps/optimism pointer unchanged, test-35 blob unchanged — all confirmed by the reviewer). r4 does NOT change the approved D1–D9 + D7a mechanism; it closes five implementation-acceptance gaps, each grounded in the real fb43bf28 source and each turned into explicit acceptance criteria + test IDs so Stages 2.0/3.0 can implement and Stage 5.0 can verify item by item: (1) capture-window isolation of the new create_recipients state; (2) corrected 7702/DELEGATECALL acceptance basis (JOVIAN⇒PRAGUE⇒7702 active); (3) original-NT2 + rule-end end-to-end coverage on the real screening path; (4) full D7a evidence incl. internal CREATE2 nonce-overflow and exact created-address asserts; (5) per-SHA cargo nextest evidence contract.
§4 D-ISO — the new create-recipient bookkeeping obeys the inactive no-op invariant [NEW DECISION r4; item 1]
Finding (grounded): at fb43bf28 the create hook (rcs_capture.rs:400-426) pushes create_recipients and derives the created address + reads the journal BEFORE any active check, and create_end (:428-450) pops + debug_assert_eq!s unconditionally. Every other mutation is gated with 'if !self.active { return; }' (observe_native_transfer:193, record_real_log:237, push_checkpoint:244, pop_checkpoint:255), so this is the one path that allocates/mutates capture-private state on inactive/gasless runs — violating the strict no-op asserted at rcs_capture.rs:367-370 and AC1. assert_capture_state_is_clean(:689-697) does not check create_recipients; the inactive test(:507-521) never drives the real create hook.

Decision: the create-recipient bookkeeping is capture-window-scoped exactly like all other capture state. (1) create performs no create_recipients mutation and no created-address derivation while inactive (early 'if !self.active { return None; }' or an active-gated helper). (2) create_end performs no pop and no debug_assert while inactive; push/pop stay balanced via the identical gate. (3) start/finish/abort already clear() create_recipients; the clean-state assertion must include it. (4) zero-endowment must not push a placeholder on the inactive path. Strictly a scoping correction to D7a bookkeeping; no observable change inside an active window; within approved scope, no new Jira.
§8.r4 — named test IDs for the five items (authoritative mapping for 2.0/3.0/5.0)
Crate paths: capture-stream + real-OpEvm hook tests in crates/builder/src/flashblocks/rcs_capture.rs #[cfg(test)]; screening/matcher end-to-end tests in crates/rcs-filter/src/integration_tests.rs (the only covering tests for screen_tx per codegraph); gating loop tests in context.rs.

Item 1: T1a inactive inspector + real create hook (zero & non-zero) → no create_recipients mutation / no journal read / empty stream; T1b gasless/disabled + CREATE tx → zero capture-private mutation; T1c assert_capture_state_is_clean also asserts create_recipients.is_empty() on every exit; T1d push/pop balance across success/child-revert/early-failure/abort/consecutive-txs.
Item 2: T2a 7702 delegated-origin tx (origin runs nested CALL + nested CREATE) → correct transfers; T2b DELEGATECALL chain with a REAL inner CALL and a value-carrying inner CREATE → inner transfers observed (empty-inner test retained but insufficient); T2c hardfork-basis evidence (JOVIAN⇒PRAGUE⇒7702 active) or exact disable config/deny-path.
Item 3: T3a original NT2 (internal CREATE) denied through real screen_tx by a transfer.* rule, RED→GREEN; T3b one rule denies BOTH top-level and internal; T3c legacy value rule fires under superset log set + dual-alias N²→(N+1)² + budgets deny at limit+1; T3d same/different audit_type action-merge per (audit_type, log_index), no empty-action Audit; T3e first-screen vs re-simulation consistency; T3f explicit statement that no matching.rs/handle.rs production change is required (comment-only diff + generic matcher unit tests are NOT end-to-end evidence).
Item 4: T4a internal CREATE2 nonce-overflow revokes phantom endowment; T4b a revoked observation STILL consumes budget (root + revoked failed-create ⇒ limit+1 denies, sticky counter); T4c successful root CREATE asserts EXACT created address == outcome.address; T4d successful internal CREATE and internal CREATE2 assert EXACT created address; T4e release-behavior (not debug_assert-only), keep root-V/child-W rollback tests. No top-level CREATE2 is fabricated (standard txs have none).
§8.E — per-SHA evidence contract [NEW r4; item 5]
GitLab pipeline 14799173 is bound to fb43bf28 and succeeded, but its 6 jobs are setup/format/clippy + 3 security checks — NO product-test job — so '146/146 + context 12/12' are unverified claims. Stage 3.0/5.0 MUST produce, for the NEW MR head SHA (not fb43bf28, not the old MR green record): (1) a readable per-SHA cargo nextest log with per-test-ID (T1a–T4e + prior §8 items) pass/fail attached to the MR/review; (2) cargo +nightly fmt --all --check exit 0 and cargo clippy --all-targets --workspace -- -D warnings 0/0 at that SHA; (3) test-35 M1/M2/M3 explicitly marked retained (covering blob proven unchanged at the new SHA) vs re-tested (re-run result recorded); (4) the review request carries the new SHA + per-item disposition; a product-test job or an attached local nextest log is the evidence of record.
§13.r4 — item-by-item responses + acceptance criteria (items 1–5)
Item 1 (Accepted → D-ISO). AC1.1 inactive create frame (zero & non-zero) performs no create_recipients mutation / no derivation (T1a,T1b). AC1.2 assert_capture_state_is_clean asserts create_recipients.is_empty() on every exit (T1c). AC1.3 push/pop balanced across success/child-revert/early-failure/abort/consecutive-txs (T1d). AC1.4 observable transfer behavior inside an active window unchanged vs r3.

Item 2 (Accepted → basis corrected; tests required). The fb43bf28 comment 'OpSpecId::JOVIAN does not activate EIP-7702' (rcs_capture.rs:1940-1942) is WRONG: op-revm spec.rs:43 maps ISTHMUS|JOVIAN=>PRAGUE and PRAGUE enables EIP-7702. The only DELEGATECALL test uses an empty inner account — insufficient. AC2.1 record correct basis + remove the contradictory comment (T2c). AC2.2 7702 delegated-origin tx with nested CALL + CREATE (T2a). AC2.3 DELEGATECALL chain with real inner CALL + value CREATE (T2b). AC2.4 if 7702 deliberately disabled on the chain, exact chainspec/run-config/deny-path evidence (not a JOVIAN claim).
Item 3 (Accepted). top_level_create_with_value_emits_transfer:1580 is mislabeled 'NT2' (original NT2 is an internal CREATE); internal_create_endowment_captured_through_composite:1143 pre-exists on baseline (comment-only change) and only checks the capture stream — not valid RED→GREEN. matching.rs had a comment-only change. AC3.1 original NT2 denied through real screen_tx by a transfer.* rule, RED→GREEN (T3a). AC3.2 one rule denies both top-level and internal (T3b). AC3.3 legacy value rule under superset + dual-alias growth + budget limit+1 (T3c). AC3.4 same/different audit_type merge, no empty-action Audit (T3d). AC3.5 first-screen vs re-sim consistency (T3e). AC3.6 explicit: no matcher/handle production change required (T3f). If the original fault cannot be reproduced on the current baseline, record the exact fixture/deployment-baseline difference instead of substituting a top-level CREATE.

Item 4 (Accepted + address-scope clarification recvxPLWsBiiGd). D7a test :1678-1735 covers only internal CREATE nonce-overflow, with no budget-sticky proof, no CREATE2, and create tests assert created != caller/zero rather than the exact address; debug_assert_eq(:442) is debug-only. AC4.1 internal CREATE2 nonce-overflow revocation (T4a). AC4.2 revoked observation still consumes budget, limit+1 denies (T4b). AC4.3 successful root CREATE exact-address assert (T4c). AC4.4 successful internal CREATE and internal CREATE2 exact-address assert (T4d). AC4.5 release-behavior safety; keep root-V/child-W rollback (T4e). AC4.6 no top-level CREATE2 fabricated (standard txs have none).

Item 5 (Accepted → §8.E). AC5.1 readable per-SHA nextest log with per-test-ID results at the new head SHA. AC5.2 fmt --check 0 and clippy -D warnings 0/0 at that SHA. AC5.3 test-35 M1/M2/M3 marked retained vs re-tested. AC5.4 review request carries the new SHA + per-item disposition.

r4 scope note: all five items are implementation-acceptance corrections within the already-approved two-fix scope and the agreed CREATE-early-failure/no-op boundary. No new Jira, no new identity, no scope expansion, no change to D1–D9 + D7a. Design-stage artifact: no production/test code modified here; Stages 2.0/3.0 implement and 5.0 verifies against the tables above. approval_mode = delegated_agent; no human signature claimed.
r5 revision — Stage-5.0 round-2 MR CHANGES_REQUESTED @ 9867fd48 (CURRENT revision; supersedes r4)
Revision r5 (2026-10-13); canonical in-repo spec sha256 e21ac26a00a99853790a40c399dad31a912fd0cbb3a4935ed904c9617713e86e. r4 was design-approved and its implementation at 9867fd48 made real progress (create/create_end are D-ISO-correct — the create_recipients push + journal derivation sit AFTER the `if !self.active { return None; }` gate at rcs_capture.rs:400-425; a precise created-address assertion and real type-4/DELEGATECALL fixtures exist). But the round-2 review showed r4's acceptance was satisfiable by a partial, test-shaped-but-not-end-to-end delivery. r5 grounds every acceptance item in the ACTUAL 9867fd48 source (file:line read via GitLab MCP), adds one production decision (D7b), and makes each acceptance row a self-contained reviewer-checkable triple of test-name + file + exact-assertion + evidence-artifact, with an explicit MISSING-ANY-ONE = FAIL rule. The five round-2 findings: (1) T3a-T3f in rcs-filter/integration_tests.rs:2374-2541 hand-craft Logs (native_transfer_log:2378 = log_builder::erc20_transfer) and call screen_tx directly — none runs the real builder OpEvm → Complete(stream) → screen_tx; T3b is ERC20-vs-native, T3c lacks legacy-value+dual-alias, T3d only buffered_len==1, T3e reuses the same input. (2) six named regressions (top_level_call_with_value_emits_transfer, create_is_ok_early_return_no_endowment_leak, root_success_child_revert_keeps_root_drops_child, whole_tx_root_failure_drops_all, create_early_failure_no_address_or_event_leak, next_capture_starts_clean) are ABSENT at 9867fd48, and internal_create2_nonce_overflow_endowment_revoked:1930 pre-populates nonce:1 ⇒ EIP-684 collision (is_ok=false), not a u64::MAX nonce overflow. (3) create_end:436-443 folds e!=a into `reverted` and silently truncates; finish_capture:289-355 can then return Complete(empty), dropping a real endowment; the :962 test calls pop_checkpoint(true) directly. (4) :1672 inactive test uses a start_capture-ing helper (:1662); :1717 checks balance only after finish_capture clears; :1822 type-4 is caller()→separate-authority and checks only `from`. (5) pipeline 14814468 @9867fd48 (success) has 6 jobs — setup-toolchain, check-format, check-clippy(39225060), security-codeql, security-gitleaks, security-poisoning-analysis — NO product-test job, so 150/150 and 233/233 are unverified. r5 is still a DESIGN-stage artifact: no production/test code is changed in Stage 1.0; Stages 2.0/3.0 implement against §8.r5 and the companion plan (built on 9867fd48).
§4 D7b — a successful-CREATE address mismatch fails closed, never silently drops a real transfer [NEW DECISION r5; PRODUCTION change, within D7a scope]
At 9867fd48 create_end:436-443 computes `reverted = !is_ok() || outcome.address.is_none() || matches!((expected.flatten(), outcome.address), (Some(e),Some(a)) if e!=a)` then pop_checkpoint(reverted). On a SUCCESSFUL CREATE whose actual address a != pre-derived e, this truncates the frame; the recorded virtual log was Transfer(caller, e, value) to the WRONG recipient, so dropping it is right, but the CREATE really moved value to a and nothing re-records Transfer(caller, a, value). If that frame had no real EVM log, finish_capture:289-355 (which has no mismatch path) then returns Complete(stream) MISSING a real native movement — fail-OPEN, the opposite of AC4. Decision: a pre-derived-vs-actual address mismatch on a successful CREATE is an observation-invariant violation, handled like RealLogMismatch: (1) create_end distinguishes three cases — (a) !is_ok() → truncate (D7); (b) is_ok with address.is_none() → revoke/truncate (D7a nonce-overflow phantom); (c) is_ok with Some(a) present but e!=a → set a sticky CaptureInvariantError::CreateAddressMismatch flag, do NOT silently drop, keep sticky state until finish/abort clears. (2) finish_capture surfaces that flag as a NON-Complete InvariantViolation so context.rs fail_closed_screen maps it to Screen::Deny; it must NEVER return Complete for a window with an observed successful-CREATE mismatch. (3) holds in debug AND release (a regular match/flag, never only a debug_assert!). (4) observed_count stays sticky; only indices/counts surfaced, never log content. In normal operation e==a always, so D7b is a defensive fail-closed guard, not an expected runtime path. Scope: D7a already owns creation-completion validity gated on CreateOutcome.address and the round-1 address-scope clarification already placed exact-address checking inside the approved scope; D7b only changes HOW a detected mismatch is handled — from a silent fail-open truncation to a fail-closed invariant — adding no new transfer semantics, no new Jira, no scope expansion. Rejected (R-G): keep the silent truncation + rely on debug_assert_eq! — release-unsafe and fail-open; the exact defect round-2 flagged.
§8.r5 — authoritative acceptance checklist (supersedes §8.r4; grounded in real 9867fd48 source). FAIL rule: missing / renamed-away / #[ignore]d / weakened ANY named test, or missing ANY §8.E evidence item, is a FAIL for the whole re-submission — a subset is a FAIL.
Item 1 — real capture→screening (home: crates/builder, the only crate that runs OpEvm AND calls rcs_filter::FilterHandle::screen_tx; codegraph: execute_best_transactions(context.rs:707)→run_tx_loop(context.rs:736)→screen_tx(handle.rs:273)). Required NEW tests, each run real OpEvm → Complete(stream) → screen_tx: r5-T3-REAL-NT2 (internal CREATE non-zero endowment → Deny; RED pre-D1, GREEN at 9867fd48), r5-T3-REAL-TOP (top-level native → same transfer.* rule → Deny), r5-T3-REAL-ONERULE (one tx, both top+internal, one rule → Deny; both distinct, no dedup), r5-T3-REAL-BUDGET (real stream limit+1 → LimitExceeded→Deny), r5-T3-REAL-RESIM (same real stream: screen_tx == screen_existing). r5-T3-KEEP: rcs-filter t3a-t3f RETAINED as generic matcher coverage but NOT labelled original-NT2 RED→GREEN.

Item 2 — restore + fix mislabel + true overflow (rcs_capture.rs): restore by exact name r5-R-CALLV top_level_call_with_value_emits_transfer, r5-R-ROOTCHILD root_success_child_revert_keeps_root_drops_child, r5-R-WHOLETX whole_tx_root_failure_drops_all, r5-R-EARLYFAIL create_early_failure_no_address_or_event_leak, r5-R-NEXTCLEAN next_capture_starts_clean, r5-R-ISOK create_is_ok_early_return_no_endowment_leak (CREATE caller nonce u64::MAX → is_ok=true/address=None, phantom revoked, surviving root V, sticky counter). NEW true overflow: r5-OVF-CREATE create_nonce_overflow_is_ok_true_address_none_revoked and r5-OVF-CREATE2 create2_nonce_overflow_is_ok_true_address_none_revoked (assert is_ok()==true AND address.is_none()). r5-REN-COLLISION: rename internal_create2_nonce_overflow_endowment_revoked → internal_create2_eip684_collision_endowment_revoked (nonce:1→is_ok=false); existing collision-budget tests may stay.

Item 3 — D7b fail-closed (rcs_capture.rs): r5-D-MM-PROD production change (create_end 3-way + finish_capture + CaptureInvariantError::CreateAddressMismatch; never Complete(empty); debug AND release). r5-D-MM-GUARD create_address_mismatch_through_real_create_end_fails_closed: a #[cfg(test)] seam forces e!=a on a real successful CREATE; assert finish_capture → InvariantViolation{CreateAddressMismatch} (not Complete) AND downstream screen_tx → Deny; FALSIFIABLE (deleting the D7b branch must make it FAIL). Replaces the :962 direct-pop_checkpoint test.

Item 4 — T1/T2 proof boundary (rcs_capture.rs): r5-T1-INACTIVE inactive_create_no_mutation_and_no_derivation_at_create_return (assert at the create return BEFORE create_end that active==false, create_recipients/checkpoints untouched, derivation did not run — via a #[cfg(test)] seam/counter); r5-T1-BALANCE create_recipients_balanced_before_finish_capture (assert empty inside the active window BEFORE finish_capture, so a missing pop is caught); r5-T1-ABORT abort_capture_mid_window_leaves_state_clean; r5-T1-CONSEC two_consecutive_txs_capture_independently; r5-T2-ORIGIN eip7702_origin_itself_delegated_runs_inner_call_and_create (ORIGIN itself carries the delegation designation; inner CALL+CREATE; assert from==origin, correct to, correct value, real success); r5-T2-KEEP existing type-4/DELEGATECALL tests retained but insufficient; r5-T2-SPEC no contradictory JOVIAN-no-7702 comment remains (JOVIAN⇒PRAGUE⇒EIP-7702 active, op-revm spec.rs:43).
§8.E — per-SHA evidence contract (round-2 item 5; updated for the 9867fd48→new-SHA basis, workspace-wide)
For the NEW MR head SHA (not 9867fd48/fb43bf28/any old green record), Stage 3.0/5.0 MUST produce: (1) a readable, accessible per-SHA cargo nextest RAW log covering the touched packages WORKSPACE-WIDE — at minimum xlayer-builder AND rcs-filter (e.g. `cargo nextest run -p xlayer-builder -p rcs-filter`) — including the exact command, the process EXIT CODE, and NAMED per-test-ID results for every r5-* test plus the retained items; a bare count with no raw log is NOT evidence. (2) workspace-wide `cargo +nightly fmt --all --check` (exit 0) and `cargo clippy --all-targets --workspace -- -D warnings` (0/0), each with command + exit code. (3) MR-table ↔ source: the MR description's test table must list ONLY tests that exist in the source at the new SHA by exact name (the 9867fd48 table listed six deleted tests — that defect must not recur). (4) test-35 M1/M2/M3 explicitly marked retained (covering blob proven byte-identical at the new SHA) vs re-tested (re-run result recorded). (5) the review request carries the new SHA + the §8.r5 per-item disposition; an attached local nextest raw log is the evidence of record even if the pipeline still lacks a product-test job.
§13.r5 — five-item disposition (source: starport-rcs-mr77-9867fd48-changes-20261013, Bus recvxT6698u7ZP; verbatim preserved in Terran ledger reviews/mr77-review-9867fd48-20261013.md remote 8305fbc)
1) T3 hand-crafted (integration_tests.rs:2374-2541) → Accepted: real-OpEvm→screen_tx tests r5-T3-REAL-* in crates/builder; rcs-filter t3a-t3f retained but not the NT2 RED→GREEN. 2) six regressions absent + collision mislabel (:1930 nonce:1→is_ok=false) → Accepted: restore all six by name incl. the is_ok=true/address=None create_is_ok_early_return_no_endowment_leak; rename the collision test; add TRUE CREATE+CREATE2 u64::MAX overflow (is_ok=true/address=None). 3) create_end:436-443 silent truncation → Complete(empty) → Accepted: new D7b production fail-closed (CreateAddressMismatch→InvariantViolation→Deny, debug+release) + falsifiable real-guard regression. 4) :1672 helper start_captures, :1717 after-finish check, :1822 origin→separate-authority → Accepted: inactive+no-derivation at create return before create_end; balance before finish_capture; abort/early-failure/consecutive; origin-itself-delegated 7702 with from/to/value + success. 5) pipeline 14814468 no product-test job; MR table names deleted tests → Accepted: workspace-wide per-SHA nextest raw log + fmt/clippy at the SHA; MR table↔source exact; test-35 M1/M2/M3 retained-vs-re-tested. r5 scope: adds exactly one production decision (D7b, within D7a scope) + a mechanically-verifiable §8.r5 acceptance; no new Jira/identity, no scope expansion, no change to D1-D9+D7a+D-ISO. Design-stage only; approval_mode=delegated_agent; a design PASS authorizes neither merge, deploy, release, MR-approve, nor scope expansion.
r6 revision — r5 design CHANGES_REQUESTED: four contradiction fixes (CURRENT revision; supersedes r5)
Revision r6 (2026-10-13); canonical in-repo spec sha256 b4303bf8f2ac02717fa93a898a2f16b3d2079225a7fe48deea309b851d492a59. The building ACCEPTED the r5 direction but rejected the r5 acceptance table because rows contradicted the real 9867fd48 call paths. r6 keeps D7b and every r5 acceptance item and fixes the four contradictions, re-grounded in the real 9867fd48 source (context.rs fail_closed_screen :110-160 + the screen_tx call site :987-1000; handle.rs screen_existing :463-568). r6 adds NO new design decision beyond r5's D7b. (1) D7b spans rcs_capture.rs AND context.rs: fail_closed_screen is an exhaustive match whose only InvariantViolation arm is RealLogMismatch, and its comment :146-149 states a new CaptureInvariantError kind forces a compile-time decision there — so the new CreateAddressMismatch REQUIRES a new production arm in context.rs (→ bounded error! + record_local_deny(ObservationInvariant) + Some(Screen::Deny)), plus the rcs_capture.rs create_end/finish_capture changes. At context.rs:987-1000 screen_tx is called ONLY for Complete; every non-Complete outcome goes to fail_closed_screen and NEVER reaches screen_tx, never enters pending, never commits. So §5 is corrected (production change is NOT confined to rcs_capture.rs); the D-MM guard asserts create_end→finish_capture InvariantViolation{CreateAddressMismatch}→fail_closed_screen→Screen::Deny via LAST_SCREEN_DECISION (screen_tx NOT called); and the budget tests split into native-capture budget (max_native_transfers_per_tx → LimitExceeded → fail_closed → Deny, pre-Complete) vs matcher-binding budget (max_event_bindings_per_tx, inside screen_tx on a real Complete(stream)). (2) screen_existing (handle.rs:463) is PRIVATE; the Approved path :502-563 recomputes quota_hash and returns AuditApproved only when it equals the stored hash, else Drop; first audit is AuditPending. So RESIM drives the PUBLIC screen_tx twice (first→AuditPending; pool→Approved; second→AuditApproved iff re-sim actions/quota_hash match) + a changed-log negative control → Drop; never a literal screen_tx==screen_existing, never a state-machine edit. (3) Acceptance IDs are classified TEST/PROD/SPEC/KEEP with an ID→artifact→evidence map (r5's 'every r5-* is a named test' was self-contradictory — some IDs are production changes or assertions); Item 1 gains explicit named tests for legacy tx-level value + Transfer ABI, dual-alias N²→(N+1)² + limit+1 deny, and same/different audit_type action-merge per (audit_type, log_index) with no empty-action Audit. (4) r6-T3-REAL-NT2's RED anchor is CONDITIONAL: if the original internal-CREATE NT2 cannot reproduce on the pre-D1 baseline, Stage 3.0 records the exact fixture/deployment/baseline difference + result (r4 §13 AC3.6) and never substitutes a top-level CREATE. r6 is still a DESIGN-stage artifact: no production/test code changed in Stage 1.0.
§8.r6 — acceptance corrections (supersede the contradicting §8.r5 rows) + ID classification
ID classes (fixes r5's self-contradictory 'every r5-* is a named test'): TEST = a named #[test] fn that must exist AND assert the stated property (checked via the per-SHA nextest log); PROD = a production code change that must appear in the MR diff; SPEC = a design assertion the MR/plan must state; KEEP = an existing test retained unchanged. Overall acceptance = every TEST present+passing ∧ every PROD in the diff ∧ every SPEC stated ∧ every KEEP retained; missing any one of any class = FAIL. Stage 3.0 publishes an ID→artifact→evidence table.

Correction A (D7b builder mapping + budget split): r6-D-MM-PROD-CTX (PROD) — context.rs fail_closed_screen :110-160 gains the InvariantViolation{CreateAddressMismatch} arm → bounded error!(tx_hash[+index/count only]) + record_local_deny(ObservationInvariant) + Some(Screen::Deny) (compiler-mandatory). r6-D-MM-GUARD (TEST) — force e!=a on a real successful create_end via a #[cfg(test)] seam; assert finish_capture⇒InvariantViolation{CreateAddressMismatch} and the loop decision Screen::Deny via LAST_SCREEN_DECISION, with screen_tx NOT called, no BufferEntry/pending, no commit; falsifiable (deleting the arm/branch fails compile or test). r6-T3-REAL-BUDGET-NATIVE (TEST) — real stream, max_native_transfers_per_tx=N, N+1 obs ⇒ LimitExceeded ⇒ fail_closed ⇒ Deny without screen_tx. r6-T3-REAL-BUDGET-MATCHER (TEST) — real Complete(stream) to screen_tx, max_event_bindings_per_tx exceeded at limit+1 ⇒ Deny. (Supersedes the single r5-T3-REAL-BUDGET row.)

Correction B (RESIM via public screen_tx): r6-T3-REAL-RESIM (TEST) — drive the PUBLIC screen_tx twice on the real captured stream: first→AuditPending (NotSubmitted); pool→Approved; second→AuditApproved because quota_hash::encode_and_hash(&actions)==stored hash. Negative control: a changed-log re-sim → different quota_hash → Screen::Drop. Do NOT assert screen_tx==screen_existing (screen_existing is private, handle.rs:463); do NOT edit the production state machine. (Supersedes the r5-T3-REAL-RESIM row.)

Correction C (Item-1 explicit named matcher tests, in crates/rcs-filter/src/integration_tests.rs with real-superset-shaped inputs): r6-T3-LEGACYVAL (TEST) legacy_value_rule_still_fires_under_superset_log_set — a legacy tx-level value-condition rule still fires under the new superset log set. r6-T3-DUALALIAS (TEST) dual_alias_transfer_rule_binding_growth_and_limit_plus_one_denies — same alias twice ⇒ N²→(N+1)²; max_event_bindings_per_tx denies at limit+1. r6-T3-AUDITMERGE (TEST) audit_type_action_merge_per_audit_type_log_index_no_empty_action — same-audit_type ⇒ ONE action per (audit_type,log_index), asserting ACTION CONTENT (not just buffered_len==1); different-audit_type each carry their own; no empty-action Audit. r5-T3-KEEP (KEEP) rcs-filter t3a-t3f retained as generic matcher coverage, not relabelled original-NT2 RED→GREEN.

Correction D (honest-evidence): r6-T3-REAL-NT2 (TEST, supersedes r5 wording) — real OpEvm internal-CREATE non-zero endowment → Complete(stream) → real screen_tx with a transfer.* deny rule ⇒ Screen::Deny at 9867fd48. The RED anchor is CONDITIONAL: show RED→GREEN only if the original NT2 reproduces on the specified pre-D1 baseline; otherwise record the exact fixture/deployment/baseline difference + result (r4 §13 AC3.6), do NOT write 'RED pre-D1, GREEN at 9867fd48' as fact, and never substitute a top-level CREATE for the internal CREATE.
§13.r6 — r5 CHANGES_REQUESTED four-item disposition (source: starport-rcs-design-review-r5-changes-20261013, Bus recvxThvIciWWP)
1) r5 said D7b's only prod site is rcs_capture.rs with finish_capture unchanged, and the guard asserted 'downstream screen_tx→Deny' — but fail_closed_screen (context.rs:110-160) is exhaustive (comment :146-149 forces a compile-time arm) and the call site :987-1000 calls screen_tx only for Complete; r5-T3-REAL-BUDGET conflated native vs matcher-binding budgets → Accepted: §0.4(1), §4 D7b (updated), §5 (updated), §8.r6 Correction A. 2) screen_existing (handle.rs:463) is private; Approved path :502-563 recomputes quota_hash, first audit is AuditPending → Accepted: §8.r6 Correction B (RESIM via public screen_tx + negative control). 3) §8.r5 omitted explicit mappings for legacy value+Transfer ABI, dual-alias N²→(N+1)²/limit+1, audit_type action-merge, and 'every r5-* is a named test' contradicted non-test IDs → Accepted: §8.r6 ID classification + Correction C (named fns + precise assertions). 4) risk of writing 'RED pre-D1, GREEN at 9867fd48' as fact / top-level-CREATE substitute → Accepted: §8.r6 Correction D (conditional RED anchor, r4 §13 AC3.6). r6 adds no new design decision beyond r5's D7b; D1-D9+D7a+D-ISO+D7b unchanged in substance; no new Jira/identity/scope. Design-stage only; approval_mode=delegated_agent; a design PASS authorizes neither merge, deploy, release, MR-approve, nor scope expansion.
r7 revision — r6 design CHANGES_REQUESTED: two remaining fixes (CURRENT revision; supersedes r6)
Revision r7 (2026-10-13); canonical in-repo spec sha256 ed1e3dafe340eed7e9023b3f661eb04656dc86bd2c005d5c463c0f536393d84c. The building ACCEPTED r6's corrections A (D7b builder mapping + budget split), B (RESIM via public screen_tx), D (honest-evidence) and the TEST/PROD/SPEC/KEEP classification; D1-D9/D7a/D-ISO/D7b are settled. Two items remained, both grounded in the real 9867fd48 source (incl. rcs-filter/Cargo.toml). r7 adds NO new design decision. (1) §8.r6 Correction C is corrected: the LEGACYVAL/DUALALIAS/AUDITMERGE tests must assert on the REAL OpEvm capture → finish_capture → Complete(stream) → screen_tx output, re-homed to crates/builder (the only crate that runs OpEvm AND calls rcs_filter::FilterHandle::screen_tx; rcs-filter/Cargo.toml has no builder/OpEvm dependency so it cannot produce a real capture stream). Named fns: legacy_value_rule_still_fires_on_real_capture_superset_stream, dual_alias_binding_growth_and_matcher_limit_plus_one_denies_on_real_stream, audit_type_action_merge_action_content_no_empty_audit_on_real_stream. The hand-crafted rcs-filter t3a-t3f stay supplementary KEEP only. (2) §5 is ACTUALLY rewritten (body §5 block + this mirror), not just claimed updated: production change spans rcs_capture.rs AND context.rs; create_end three-way branch; finish_capture gains the CreateAddressMismatch non-Complete branch (RealLog cross-check semantics unchanged — it is NOT 'finish_capture unchanged'); context.rs fail_closed_screen gains the compiler-mandatory arm; the r3/r4 'only production change site = rcs_capture.rs' / 'context no structural change' / 'net localized to rcs_capture.rs' phrasing is marked SUPERSEDED. r7 is still a DESIGN-stage artifact: no production/test code changed in Stage 1.0.
§13.r7 — r6 CHANGES_REQUESTED two-item disposition (source: starport-rcs-design-review-r6-changes-20261013, Bus recvxTnGhvAaCQ)
1) §8.r6 Correction C put LEGACYVAL/DUALALIAS/AUDITMERGE in rcs-filter with shape-only inputs → Accepted, re-homed to crates/builder (§0.5(1), §8.r6 Correction C r7).
2) r6 claimed §5 updated but still carried r3/r4 clauses → Accepted, §5 rewritten (§0.5(2)).

r7 scope note: r7 adds no new design decision beyond r5's D7b; wires Item-1 to real builder capture→screen_tx and corrects §5 blast-radius. No new Jira/identity/scope; D1-D9+D7a+D-ISO+D7b unchanged. Still a design-stage artifact; approval_mode = delegated_agent; design PASS authorizes neither merge/deploy/release/MR-approve/scope expansion.

---

## r8 revision — Stage-5.0 round-3 MR CHANGES_REQUESTED: five P2 acceptance-strengthening items

Revision r8 (2026-10-13); canonical in-repo spec sha256 cce8b3acb09e057bae8781bbf4b24b70f64b28bb8f645b73e6b7150f637360af; MR77 head 21a311e9. r7 was design-PASSed but its implementation at 21a311e9 FAILED the Stage-5.0 round-3 delegated MR acceptance (CHANGES_REQUESTED starport-rcs-mr77-21a311e9-changes-20261013, Bus recvxUgPFw0MdR with reply recvxUidSBS4ej). The building found five P2 gaps between the r7 acceptance contract and the 21a311e9 source. r8 does NOT change the approved D1-D9+D7a+D-ISO+D7b mechanism; it adds mechanically-verifiable acceptance IDs for each round-3 item, grounded in the actual 21a311e9 source.

### Five items (full detail in §0.6 + §8.r8 + §13.r8 of canonical in-repo spec):

1. advance_to_approved_for_test isolation (handle.rs:594-601): gate behind #[cfg(test)] so production builds do not expose the symbol (PROD r8-ADVANCE-CFG; TEST r8-ADVANCE-TEST).

2. T3 tests on real builder path assertion strengthening: the tests at 21a311e9 already use real OpEvm→screen_tx in crates/builder, but three assertions need strengthening — legacy-value negative control (r8-T3-LEGACYVAL), dual-alias N²→(N+1)² growth proof (r8-T3-DUALALIAS), audit_type ActionItem content assertion (r8-T3-AUDITMERGE). Original-NT2 RED/GREEN evidence required (r8-T3-NT2-EVID).

3. Fail-closed loop completeness: D-MM guard test (:2115) ends at InvariantViolation — extend to loop-level LAST_SCREEN_DECISION=Deny + screen_tx NOT called (r8-DMM-LOOP); add address_mismatch_create_index cleanup check to abort test (r8-DMM-ABORT).

4. T1/T2 and overflow proof boundaries: derivation_ran hardcoded false→real computed value (r8-T1-DERIVE); overflow tests add CreateOutcome.is_ok()/address.is_none() assertions (r8-OVF-ASSERT); T2 origin test add per-transfer to/value/success assertions (r8-T2-ORIGIN); six regressions retained (r8-REGRESS-KEEP).

5. Evidence and corrections: workspace-wide per-SHA nextest raw log with ID→test-name map (r8-EV-LOG); MR table corrected with r8 label, no stale names (r8-EV-TABLE); pipeline 14825765 clippy job 39249795 recorded as CI evidence (r8-EV-CI); test-35 M1/M2/M3 marks (r8-EV-T35); superpowers preserved (r8-EV-SUPERPOWERS).

### §13.r8 — five-item disposition (source: starport-rcs-mr77-21a311e9-changes-20261013, Bus recvxUidSBS4ej)

| # | Finding (grounded 21a311e9) | r8 disposition | Where |
|---|---|---|---|
| 1 | advance_to_approved_for_test public, not #[cfg(test)] | Accepted → §8.r8 Item 1 (PROD+TEST) | §0.6(1), §8.r8 |
| 2 | T3 tests need assertion strengthening (legacy-value neg ctrl, N²→(N+1)² proof, ActionItem content) + NT2 RED evidence | Accepted → §8.r8 Item 2 (TEST+SPEC) | §0.6(2), §8.r8 |
| 3 | D-MM guard no loop-level Deny assert; abort test no mismatch-flag check | Accepted → §8.r8 Item 3 (TEST) | §0.6(3), §8.r8 |
| 4 | derivation_ran hardcoded false; overflow no is_ok/address assert; T2 no to/value/success | Accepted → §8.r8 Item 4 (PROD+TEST+KEEP) | §0.6(4), §8.r8 |
| 5 | Per-SHA log missing; MR table r4→r8; CI 404 claim incorrect | Accepted → §8.r8 Item 5 (SPEC/KEEP) | §0.6(5), §8.r8 |

r8 scope note: no new design/production decision; D1-D9+D7a+D-ISO+D7b unchanged. Plan baseline = 21a311e9 (NOT 9867fd48, NOT 714e1293). approval_mode = delegated_agent; design PASS authorizes neither merge/deploy/release/MR-approve/scope expansion.
r9 revision — r8 design CHANGES_REQUESTED: two corrections (CURRENT revision, superseding r8)
Revision r9 (2026-10-13); canonical in-repo spec sha256 7321a778b5c9bbe1fdb47ea296a109a9a091a8124e34f21729d6e98d4ea5a26d. The in-repo spec is at docs/superpowers/specs/2026-10-12-rcs-native-transfer-mr77-fixes-design.md. MR77 head 21a311e93d624e6a252c16952680437c8d2c5525. Plan baseline 21a311e9 (unchanged from r8).

r8 design CHANGES_REQUESTED (starport-rcs-design-review-r8-changes-20261013, Bus recvxUBDyttm2E): building accepted the five r8 directions but found two defects. r9 does NOT change D1–D9/D7a/D-ISO/D7b; it corrects:

Correction A — test-utils Cargo feature replacing bare #[cfg(test)] for advance_to_approved_for_test cross-crate isolation. The building correctly identified that #[cfg(test)] on rcs-filter is inactive when builder compiles rcs-filter as a dependency (even during cargo test -p xlayer-builder). r9 uses a non-default [features] test-utils = [] on rcs-filter, gated via #[cfg(any(test, feature = "test-utils"))], enabled by builder's [dev-dependencies] rcs-filter = { path = "../rcs-filter", features = ["test-utils"] }. Production cargo build -p rcs-filter MUST NOT expose the symbol.

Correction B — complete r9 clauses published verbatim into this Lark doc body (see below). The building noted the r8 Lark section contained only summaries with a pointer to the in-repo spec — the reviewer could not independently confirm the acceptance clauses. r9 publishes the COMPLETE §8.r9 and §13.r9 content below.

Scope: NO change to D1–D9/D7a/D-ISO/D7b mechanism, the 19 acceptance IDs (all preserved, renumbered r8-* → r9-*), the plan baseline 21a311e9, or the Lark doc token. Design-stage only — no production/test code modified. approval_mode = delegated_agent.
Section 8.r9 - r9 Acceptance IDs (19 IDs, 5 items, FAIL rule: any missing/renamed/ignored/weakened TEST, missing PROD, unstated SPEC, or deleted KEEP = FAIL). All IDs carry forward from r8 renumbered r8-* to r9-*. Only r9-ADVANCE-CFG substantively changed (Correction A: test-utils feature). Source: 21a311e9.
Item 1 - advance_to_approved_for_test isolation (PROD+TEST, CORRECTED r9): r9-ADVANCE-CFG PROD: gate pub fn advance_to_approved_for_test behind cfg(any(test, feature = test-utils)). Add [features] test-utils = [] (non-default) in rcs-filter/Cargo.toml. Add rcs-filter = { path = ../rcs-filter, features = [test-utils] } in builder/Cargo.toml [dev-dependencies]. cargo build -p rcs-filter (production) MUST NOT expose symbol. cargo test -p xlayer-builder MUST compile and pass. r9-ADVANCE-TEST TEST: RESIM test at crates/builder/rcs_capture.rs:2444 must compile and pass; FilterHandle::for_test constructor remains public.
Item 2 - T3 tests on real builder path (TEST+SPEC): r9-T3-LEGACYVAL (TEST, :2468): negative control zero ScreenInput.value same stream yields Allow; prove rule fires via tx-level value binding not only transfer.value. r9-T3-DUALALIAS (TEST, :2501): two-stream comparison proving checked_binding_count N-squared to (N+1)-squared growth before budget=3 denies. r9-T3-AUDITMERGE (TEST, :2538): assert ActionItem content - each action's audit_type, bound transfer.from/to/value; action list NOT empty beyond buffered_len. r9-T3-NT2-EVID (SPEC, :2291): MR/plan must state RED on pre-D1 baseline or non-repro record with exact fixture difference (per r4 par 13 AC3.6).
Item 3 - Fail-closed loop completeness (TEST): r9-DMM-LOOP (TEST, :2115 in builder cfg(test)): extend D-MM guard to drive builder candidate-loop path after InvariantViolation; assert LAST_SCREEN_DECISION reads Some(Screen::Deny), screen_tx NOT called, no BufferEntry pending created. r9-DMM-ABORT (TEST, :3041): extend abort test to assert address_mismatch_create_index.is_none() after abort_capture; sticky flag cleaned on BOTH finish_capture (:328) AND abort_capture (:413).
Item 4 - T1/T2 and overflow proof boundaries (TEST+PROD+SPEC+KEEP): r9-T1-DERIVE (PROD+TEST, :448-451 prod + :2974 test): change derivation_ran from literal false to computed value (true when journal read for address derivation, false sentinel). r9-OVF-ASSERT (TEST, :2913 and :2944): explicit assertions CreateOutcome.result.is_ok() and CreateOutcome.address.is_none() before stream emptiness assertion. r9-T2-ORIGIN (TEST, :3113): per-transfer assertions: CALL log to equals call_recipient 0xE4 and value equals 1 wei; CREATE log to NOT zero/caller/call_recipient AND value equals 1 wei; assert result.result.is_success(). r9-REGRESS-KEEP (KEEP, :2591-2856): retain all six regressions by name passing.
Item 5 - Evidence and corrections (SPEC/KEEP): r9-EV-LOG (SPEC): readable raw cargo nextest log at new head SHA covering xlayer-builder AND rcs-filter workspace-wide, with command, exit code, ID to actual test name map. r9-EV-TABLE (SPEC): MR test table lists only existing tests by exact name; design label r9. r9-EV-CI (SPEC): pipeline 14825765 clippy job 39249795 recorded as CI format/clippy evidence (NOT substitute for product-test log). r9-EV-T35 (SPEC/KEEP): test-35 M1/M2/M3 marks retained-vs-re-tested. r9-EV-SUPERPOWERS (KEEP): preserve docs/superpowers, .superpowers, checkpoint tarballs, Terran review ledgers.
Section 13.r9 - r8 CHANGES_REQUESTED feedback and r9 disposition
r10 revision (2026-10-13) — r9 CHANGES_REQUESTED (Bus recvxUH7IxcuCV): Three assertion-wording items strengthened. Corrections A+B ACCEPTED in r9 carry forward. D1-D9/D7a/D-ISO/D7b mechanism UNCHANGED.
Canonical in-repo spec sha256: 87cb906d8da9422db3c5d75ecf4464cf84b5e20b62e5725419596c0e5d203c68
MR head: 21a311e93d624e6a252c16952680437c8d2c5525
r10 Item 1 — T3-DUALALIAS + T3-AUDITMERGE Strengthened Assertions
r10-T3-DUALALIAS (TEST): Two-stream comparison with distinguishable budget-boundary controls. Stream-A: 1 native transfer, within budget → Allow or Audit. Stream-B: 2 native transfers, (N+1)² ≥ max_event_bindings_per_tx=3 → Deny. OR assert actual budget error path + binding count. A bare rule-itself Deny cannot satisfy the assertion.
r10-T3-AUDITMERGE (TEST): ALL of: (a) same audit_type dedup to ONE action per (audit_type, log_index) with bound transfer.from/to/value; (b) different audit_type each retained as separate actions; (c) NEGATIVE CONTROL — all-null or no physical event → NO empty Audit action. audit_type is the key of the actions mapping (matching.rs:163-171), NOT a field of ActionItem.
r10 Item 2 — T2-ORIGIN Exact Address Match
r10-T2-ORIGIN (TEST): CREATE recipient equals EXACTLY the real CreateOutcome.address (or verified nonce-derived address). Any wrong fourth address MUST fail the test. Additionally: CALL log → to==call_recipient, value==1 wei; CREATE log → to==deployed address, value==1 wei; result.is_success() asserted.
r10 Item 3 — Inherited Completeness Written Back into IDs
r10-DMM-LOOP (TEST): Full loop Deny chain — InvariantViolation→fail_closed_screen→Deny. LAST_SCREEN_DECISION=Some(Screen::Deny). screen_tx NOT called. No pending, no commit. Falsifiable: removing CreateAddressMismatch arm from fail_closed_screen must fail.
r10-DMM-ABORT (TEST): Set mismatch flag to Some → finish_capture clears (is_none). Repeat: set to Some → abort_capture clears (is_none). BOTH paths verified.
r10-OVF-ASSERT (TEST): outcome.result.is_ok()==true AND outcome.address.is_none()==true on real CreateOutcome. PLUS sticky observed_count equals pre-overflow count.
r10-T1-DERIVE (PROD+TEST): derivation_ran computed: true when journal was read for derivation; false as not-hit sentinel. Separate "hook hit" counter — false must NOT mean both "not run" and "ran but didn't derive."
r10-ADVANCE-CFG (PROD): #[cfg(any(test, feature="test-utils"))]. Dual-verification: (a) cargo build -p rcs-filter MUST NOT expose the symbol (default prod dep graph does NOT enable feature); (b) cargo test -p xlayer-builder MUST compile and pass.
§13.r10 — r9 CHANGES_REQUESTED Disposition
r9 CHANGES_REQUESTED (Bus recvxUH7IxcuCV, starport-rcs-design-review-r9-changes-20261013, reply_to viking-nativetransfer-starport-rcs-design-review-r9-20261013, correlation player-fix-mr77-native-transfer-2items-20261012):

Item 1: r9-T3-DUALALIAS allows rule-itself Deny; r9-T3-AUDITMERGE lacks dedup/negative-control/audit_type-as-key → ACCEPTED, strengthened with r10-T3-DUALALIAS (budget boundary both sides) and r10-T3-AUDITMERGE (same/diff dedup, all-null negative control, key-not-field).

Item 2: r9-T2-ORIGIN says NOT zero/caller/call_recipient → ACCEPTED, strengthened with r10-T2-ORIGIN (exact CreateOutcome.address; any wrong fourth address fails).

Item 3: Inherited completeness: DMM loop Deny, mismatch flag clear, overflow sticky budget, derivation sentinel, test-utils dual-verification → ACCEPTED, written into r10-DMM-LOOP, r10-DMM-ABORT, r10-OVF-ASSERT, r10-T1-DERIVE, r10-ADVANCE-CFG.

D1-D9/D7a/D-ISO/D7b mechanism UNCHANGED. Corrections A+B carry forward from r9. Scope: assertion-wording ONLY. No new design decisions. approval_mode=delegated_agent.
r8 CHANGES_REQUESTED feedback (Bus recvxUBDyttm2E): starport-rcs reviewed r8 and returned CHANGES_REQUESTED for two items. Verbatim body preserved in unit-memory ledger. Building confirmed: D1-D9/D7a/D-ISO/D7b mechanism not reopened. 21a311e9 implementation still CHANGES_REQUESTED; cleanup/finalization not yet approved. Same MR77, XLOP-1228/XLOP-1217 retained.

Item A - r8-ADVANCE-CFG cross-crate test isolation: cfg(test) alone cannot gate advance_to_approved_for_test because rcs-filter's cfg(test) is inactive when builder compiles rcs-filter as dependency. Builder test at rcs_capture.rs:2444 would fail to compile. r9 Correction A: test-utils Cargo feature per Section 0.7.

Item B - incomplete r8 Lark doc section: r8 Lark doc only has five-item summary plus pointer to in-repo spec. Reviewer cannot confirm acceptance clauses from Lark doc alone. r9 Correction B: complete Sections 8.r9 and 13.r9 published verbatim into this Lark doc body.

Round-3 MR CHANGES_REQUESTED at 21a311e9 (carried forward from r8): source building starport-rcs reply starport-rcs-mr77-21a311e9-changes-20261013 (Bus recvxUidSBS4ej). Five P2 items all accepted with r9-renumbered IDs. Item 1 ADVANCE-CFG disposition updated for Correction A. All other dispositions unchanged from r8.
r11 revision — Stage-5.0 round-4 MR a0e5d7b7 CHANGES_REQUESTED: five acceptance-strengthening items (CURRENT revision; supersedes r10)
Revision r11 (2026-10-13); supersedes r10. Canonical spec content hash (sha256, r11): a2ac22f453a4bf1b060b5072f61bda2be5364ccc6a5214295e82b9bfd0236d80. MR head: a0e5d7b73a669d50ed18f65d1c40c52a7a70a230 (round-4 commits 3a87fdc7 / e232ab0e / a0e5d7b7). Plan baseline: a0e5d7b7 (NOT 21a311e9 - updated per 5.0 round-4 head).
r10 was design-PASSed but its implementation at a0e5d7b7 FAILED the Stage-5.0 round-4 delegated MR acceptance (CHANGES_REQUESTED starport-rcs-mr77-a0e5d7b7-changes-20261013, Bus recvxVecklaw7X). The building found that most of the five P2 items are places where the implementation and evidence fall short of clauses r10 already contains, with two items (native LimitExceeded real-builder chain, per-SHA evidence contract) requiring slightly more explicit wording. r11 adds narrow, mechanically-verifiable acceptance IDs for each item while preserving r10's D1-D9/D7a/D-ISO/D7b mechanism and all r10 acceptance IDs unchanged. Corrections A+B (test-utils feature, complete Lark publication) carry forward from r9-r10.
Section 8.r11 - r11 Acceptance Additions (5 additions to r10 IDs; all r10 IDs preserved unchanged)
r11-NATIVE-LIMIT-DENY (TEST): Native transfer LimitExceeded through real builder path. Test adds candidates exceeding max_native_transfers_per_tx, drives through execute_best_transactions, asserts: Deny, no pending, no commit, no receipt, normal control Allow/Audit. Falsifiable. No hand-crafted outcomes.
r11-DERIVE-CTRL (TEST): Truly non-CREATE control alongside r10-T1-DERIVE. Control executes real tx with no CREATE at any depth (pure CALL-only), asserts hook hit counter/sentinel = 0. Inactive CREATE path separately asserts hook hit counter > 0, derivation_ran=false. Proves sentinel distinguishes not-hit from hit-then-early-returned. Per-test-case reset required.
r11-EVIDENCE-CONTRACT (SPEC): Complete per-SHA test evidence as raw output, not summaries. Required: (a) full cargo nextest run output for BOTH xlayer-builder and rcs-filter workspace-wide at submission SHA, with exact command, exit code, per-test-ID pass/fail/duration; (b) cargo tree -e features -p xlayer-node output at submission SHA, showing rcs-filter/test-utils NOT enabled in default production dep graph; (c) cargo test -p xlayer-builder output (test-utils enabled) proving cross-crate tests available. Evidence tied to exact submission SHA.
r11-NT2 (SPEC): NT2 evidence must state EITHER: (a) exact pre-fix baseline revision/SHA, fixture input, actual hook/depth observation output, and how it differs from post-fix output (GENUINE REPRODUCTION); OR (b) explicit statement scenario cannot be reproduced with available tooling, exact baseline/fixture attempted, actual result observed (HONEST NON-REPRODUCTION). Do not fabricate RED baseline or claim reproduction without experimental evidence. Acceptance table must match actual evidence status.
r11-MR-TABLE (SPEC): MR description body must: (a) list only tests that exist at submission SHA by EXACT function names; (b) drop any test names confirmed non-existent by prior review rounds; (c) label design revision as r11 (not any earlier revision); (d) link full verified Lark doc URL and canonical sha256. Pipeline evidence must note actual CI jobs that ran.
Section 13.r11 - Round-4 Five-Item Disposition
Source: starport-rcs-mr77-a0e5d7b7-changes-20261013 (Bus recvxVecklaw7X, reply_to viking-nativetransfer-starport-rcs-mr77-review-a0e5d7b7-20261013, correlation player-fix-mr77-native-transfer-2items-20261012).
Disposition: (1) Native LimitExceeded not through real builder - Accepted, r11-NATIVE-LIMIT-DENY adds execute_best_transactions Deny chain with normal control. (2) Inactive CREATE hook-hit indistinguishable - Accepted, r11-DERIVE-CTRL adds non-CREATE zero-hit control alongside r10-T1-DERIVE. (3) Per-SHA nextest raw output missing, default node feature tree not shown - Accepted, r11-EVIDENCE-CONTRACT requires full raw nextest per test ID both crates plus cargo tree default node features. (4) NT2 evidence not reproduced pre-fix baseline - Accepted, r11-NT2 requires reproduce-or-honest-non-reproduction with baseline/fixture/result. (5) MR body still shows r4, stale test names - Accepted, r11-MR-TABLE requires exact test names, r11 label, full URL and sha256.
r11 scope note: NO change to D1-D9/D7a/D-ISO/D7b mechanism. All r10 acceptance IDs preserved unchanged. r11 adds five narrow acceptance-strengthening IDs. Corrections A+B carry forward from r9 to r10. Plan baseline updated to a0e5d7b7 (current MR77 head after round-4). approval_mode = delegated_agent; design PASS authorizes neither merge, deploy, release, MR-approve, nor scope expansion.
```
