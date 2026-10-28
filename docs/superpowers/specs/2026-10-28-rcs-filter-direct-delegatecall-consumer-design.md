<!-- PUBLICATION BLOCK — excluded from the canonical hash by construction.
     Canonical content = this file from the first `# ` heading onward.
     Verify with:  sed -n '/^# /,$p' <this file> | sha256sum  -->

> **Published design document (review target):**
> https://okg-block.sg.larksuite.com/docx/Eo2ldAfN7od95ExYKAWlh2uXgAd
> — revision **r4**, canonical content sha256
> `769690d4c31098c3c4dd318cdbbc8ac7d94fa111ca4675becca1f76952be4179`.
> Durable publication record: [`docs/superpowers/lark-review-doc.md`](../lark-review-doc.md)
> (workspace-relative: `xlayer-reth/docs/superpowers/lark-review-doc.md`).
> Reviewed by building `starport-rcs` over the Terran Bus; a PASS binds to exactly this
> revision and this hash.
>
> Revision history: r1 → CHANGES_REQUESTED (`reczz28Skbf6IAfL`, selected convergence O2);
> r2 → CHANGES_REQUESTED (`reczz28Sl2W46ZVp`); r3 → CHANGES_REQUESTED (`reczz28SlQjtgpho`,
> one blocker: Phase A destroyed duplicate-alias evidence). r3/r2/r1 are retained below as
> history.

# RCS Filter Direct Delegatecall Rule Consumption and L1 Deposit Audit — Code-Level Design

================ REVISION r4 — CURRENT REVISION (supersedes r3, r2 and r1) ================

Design revision: r4
Revised: 2026-10-28
Repository: https://gitlab.okg.com/github/xlayer-reth   Branch: main (implementation lands on a NEW branch cut from it)
Inspected base SHA: d32b20beed8c8b56a2449f00a24fe26288a55d48
Local spec path: xlayer-reth/docs/superpowers/specs/2026-10-28-rcs-filter-direct-delegatecall-consumer-design.md
Jira Epic: https://okcoin.atlassian.net/browse/XLOP-1239
Source design: https://okg-block.sg.larksuite.com/docx/ODE0deAHnoyEfcxixyIlkyl4gWf (modified 2026-10-28T06:44:09Z)
Unit: medivac-directcallconsumer-starport-rcs   Building: starport-rcs
Correlation id: direct-delegatecall-units-20261028-001
Protocol version: stays 1. Convergence target: O2.

WHY r4 EXISTS: revision r3 received CHANGES_REQUESTED from `starport-rcs`
(`starport-rcs-directcallconsumer-spec-r3-changes-20261028`, record `reczz28SlQjtgpho`, reply to
`medivac-directcallconsumer-starport-rcs-stage1.0-specreview-r3`). r3's lifecycle and interface
items were closed at the design level, leaving **exactly one blocker**: r3's Phase A used
`Vec<serde_json::Value>`, which **destroys duplicate-alias evidence before V-10 can run**. r4
fixes raw-input fidelity and the related tests, and nothing else.

CLOSED BY THE r3 DECISION AND NOT REOPENED: the active-window / parked-result separation;
`finish` computing first, then clearing the window, then parking last; one-take / second-take;
`start`/`abort` clearing the stale result; native `Deny` taking precedence; the real-sequence
direction of tests 37–39 and 48–52; CT-F's empty exception list; non-empty non-ASCII aliases
being allowed; parent-frame entry-time own-code recording; `CaptureFaults` defaulting to no
faults. Everything closed earlier also stands: O2 / v1, the mixed-rule both-binding requirement,
the budgets, and the qualified `known_allowed` contract.

STANDING BOUNDARIES, PRESERVED VERBATIM: (1) a deposit already listed in `known_allowed` which
executes an unapproved implementation is **still not denied**; (2) RCS degradation / fail-open
does **not** mean "protected", so a non-`Allow` return is not an end-to-end guarantee;
(3) joint acceptance with the producer is **INTEGRATION_PENDING**. These three must appear in the
interface, in test 46, and in the final report, and may never be replaced by an unqualified
claim. No v2 / old CT-11; no old unit resumed; no old MR modified.

SCOPE OF r4 — one fix and its tests:

  F1  §6.2.1 Phase A must preserve the **original rule bytes**, so duplicate-alias evidence
      survives until V-10 runs.                                       (§6.2.1, §6.2.1a)
  F2  The duplicate/fidelity test matrix at **both** entry points, plus the corrected
      expectations for tests 33, 55, 56 and 57.                                (§6.2.3)

---

## F1 / §6.2.1 (replaces r3 §6.2.1) — Phase A preserves original bytes

### 6.2.1.0 The defect r3 had

r3 wrote `rules: Vec<serde_json::Value>` and called it "untyped on purpose". It is untyped, but
it is **not raw**: `Value::Object` is already a `Map`. Verified in the locked dependency —
`serde_json` **1.0.150** (`Cargo.lock` checksum
`e8014e44b4736ed0538adeecded0fce2a272f22dc9578a7eb6b2d9993c74cfb9`), `src/value/de.rs`, the
`KeyClass::Map` arm of the `Value` map visitor:

```rust
let mut values = Map::new();
values.insert(first_key, tri!(visitor.next_value()));
while let Some((key, value)) = tri!(visitor.next_entry()) {
    values.insert(key, value);          // <-- duplicate key OVERWRITES
}
Ok(Value::Object(values))
```

So a duplicate key is silently collapsed, last-wins, **during Phase A**. Everything downstream —
`serde_json::from_value::<RawRule>` and therefore `alias_map_reject_duplicates` — only ever sees
the deduplicated map, and **V-10 becomes unimplementable**.

**Concrete counterexample** (the reviewer's, and it is correct):

```json
"direct_delegatecalls": { "proxy1": "0x1111111111111111111111111111111111111111",
                          "proxy1": "0x2222222222222222222222222222222222222222" }
```

Phase A keeps only the second value; Phase B's visitor receives a single `proxy1`; the rule can
be **accepted**. In fixing the ordering problem, r3 reintroduced exactly the last-wins defect
that r1 and r2 had closed. That is a regression, not a detail.

### 6.2.1.1 Corrected Phase A (normative)

Phase A preserves each rule's **original JSON bytes**, with duplicates intact.

```rust
// serde_json's `raw_value` feature is REQUIRED and must be enabled for every crate that
// deserializes rules (it is a non-default feature; verified present in serde_json 1.0.150's
// Cargo.toml as `raw_value = []`).
use serde_json::value::RawValue;

#[derive(Deserialize)]
struct AuditRequestEnvelope<'a> {
    #[serde(default = "default_protocol_version")] protocol_version: u32,
    /// ORIGINAL BYTES per rule. NOT `Value`, NOT a map, NOT re-serialized.
    #[serde(borrow)] rules: Vec<&'a RawValue>,
    txs: Vec<DepositTxRequest>,
    #[serde(default)] known_allowed: Vec<String>,
    #[serde(flatten)] unknown: BTreeMap<String, serde_json::Value>,
}
```

(`Box<RawValue>` is the owned equivalent when a borrow cannot be held; either is acceptable, and
both preserve the bytes. `RawValue::get()` returns the original `&str` slice.)

The same change applies to the hot-reload wire type, so both entry points share one discipline:

```rust
#[derive(Deserialize)]
pub struct RulesResponseWire {
    pub protocol_version: u32,
    pub content_version: u64,
    pub rules: Vec<Box<RawValue>>,          // was Vec<RawRule>
    #[serde(flatten)] pub unknown: BTreeMap<String, serde_json::Value>,
}
```

**Hard prohibition (normative).** Between receiving the bytes and completing duplicate detection,
a rule MUST NOT pass through `serde_json::Value`, any `Map`, `from_value`, or any
re-serialization. Any of those silently discards the evidence V-10 depends on. `Value` remains
fine for the envelope's `unknown` map, which carries no duplicate-sensitive semantics.

### 6.2.1a Phase B on preserved bytes (normative)

Phase A's gate is unchanged and still runs first, because the envelope fields are typed while the
rules are still raw:
1. `unknown` non-empty ⇒ `-32602`, naming every unknown top-level key.
2. `protocol_version != 1` ⇒ `-32602`, naming the value and the supported set `[1]`.

Phase B then processes each rule **independently, from its preserved bytes**:

```rust
for (index, raw) in envelope.rules.iter().enumerate() {
    let src: &str = raw.get();                       // ORIGINAL bytes, duplicates intact
    // (B1) Streaming root-key scan — collects keys IN ORDER, duplicates PRESERVED.
    //      No Map is constructed, so nothing is lost. Also lifts `id` for reporting.
    let scan = scan_rule_root(src)?;                 // -> { keys: Vec<String>, id: Option<String> }
    // (B2) V-0 / V-12 from `scan.keys`.
    // (B3) Typed decode FROM THE BYTES — never `from_value`.
    let rule: RawRule = serde_json::from_str(src)?;  // V-7 / V-10 / V-11 fire here, for real
    // (B4) semantic V-1..V-6, V-8, V-9.
}
```

Per-rule order, now both ordered **and** evidence-preserving:

| Step | Checks | Why it can run here |
|---|---|---|
| B1 | — (collection only) | a streaming visitor over the original bytes; keeps duplicates |
| B2 | **V-0** unknown root key; **V-12** duplicate root key | from `scan.keys`, which still contains repeats |
| B3 | **V-7** unknown key in `callstack`; **V-10** duplicate alias; **V-11** wrong JSON type | `from_str` on the original bytes, so `alias_map_reject_duplicates` really sees both entries |
| B4 | **V-1 … V-6, V-8, V-9** | on the typed rule |

**V-12 is new** and is the same fidelity defect one level up: a duplicate rule-root key is also
last-wins and could hide a modelled field. It is detectable at zero extra cost from B1's key
list, so it is rejected rather than left silent:
`"rule declares root key '{k}' more than once"`. It is in scope because it is precisely the
raw-input-fidelity problem this revision exists to fix; if the reviewer prefers it deferred, it
can be dropped without affecting F1.

**Reporting identity.** `scan.id` is lifted in B1, i.e. **before** any typed decode, so a rule
can be named even when B3 would fail. When `id` is absent, repeated, or not a string, the rule is
reported by its **0-based index in the request**, explicitly labelled as an index. An id is never
fabricated and never inferred.

All rejections are aggregated across rules and reported together; the outcome is `-32602` with
nothing executed, installed or committed, and on the hot-reload path a `FilterError` with
last-known-good rules left in force.

### 6.2.1b What remains explicitly unpromised
A document that cannot be parsed into `AuditRequestEnvelope` at all (unbalanced braces, a
non-array `rules`, a rule element that is not a JSON object) fails with the serde error — still
`-32602`, still nothing executed — and Phase A's ordering guarantee begins only once the envelope
parses. `RawValue` does **not** change this: it still requires each rule element to be
syntactically valid JSON.

---

## F2 / §6.2.3 — Fidelity and ordering acceptance (replaces r3 §6.2.3 rows 53–58)

Every row below runs at **both** entry points — the `xlayer_auditTransactions` JSON-RPC request
**and** the `/rules` hot-reload decode — and every rejection case additionally asserts that
**nothing was executed, installed or committed**, and that on the hot-reload path the
**last-known-good rule set is unchanged**.

| # | Scenario | Expected |
|---|---|---|
| **33** | Duplicate alias key, **driven through the whole entry point** (not a visitor unit test) | rejected by **V-10**. A visitor-only unit test is explicitly **insufficient** — it is what let the r3 defect hide. |
| **33a** | Duplicate alias, **same value** both times | rejected by V-10. Identical values are still a duplicate key; the rule is not "harmlessly redundant". |
| **33b** | Duplicate alias, **different values** (the reviewer's counterexample) | rejected by V-10 — and specifically **not** silently accepted with the last value |
| **33c** | Duplicate alias with the two entries in **reversed order** | rejected by V-10, same reason text; the outcome does not depend on which value appears last |
| **33d** | Duplicate alias **plus** a V-0 unknown root key in the same rule | **V-0** reported (B2 precedes B3), and the rule is rejected; the duplicate does not vanish from the record |
| **33e** | Duplicate alias **plus** a V-4 uppercase address in the same rule | both are on the same rule; **V-10** is reported (B3 precedes B4) |
| **53** | One request with an unknown top-level key **and** `protocol_version: 2` **and** a rule with a malformed `callstack` type | `-32602` citing the **unknown top-level key** (Phase A step 1) |
| **54** | `protocol_version: 2` plus a malformed rule, no unknown top-level key | `-32602` citing the **version** (Phase A step 2) |
| **55** | Valid envelope; three rules failing differently — V-0 misspelled root key, **V-10 duplicate alias (through the full entry point)**, V-4 uppercase address | one `-32602` listing **all three**, each with its id and reason |
| **56** | The payloads of 53 and 55 with top-level keys **and** `rules` elements **reordered** | identical verdicts. Comparison rule, corrected: for rules **carrying an id**, compare the **set of reasons keyed by id**; for rules **without an id**, the reported index must track the **new** input position — the test must **not** require an index to stay unchanged under reordering. |
| **57** | A rule whose `id` is **missing**, **duplicated**, or **not a string** | the rule is localised by an **explicit 0-based index**, labelled as an index, with **no fabricated id**. Corrected from r3: the test asserts **only** the identity guarantee, **not** that the reason is V-4 — a missing or non-string `id` may legitimately surface as a typed-decode error in B3 first, since `RawRule.id` is a required `String`. |
| **58** | A rule failing both V-0 and V-10 | **V-0** reported (B2 precedes B3) |
| **60** | **Byte-fidelity guard:** assert at the type level / by test that the rule path from the wire to the duplicate check never materialises a `Value` or a `Map` for a rule — e.g. `rules` is `Vec<RawValue>` and no `from_value` call exists on that path | holds; this is the regression guard for the r3 defect itself |

Tests 37–39 and 48–52 (lifecycle) and the rest of the matrix are retained unchanged from r3.

---

## r4 evidence boundary

- The r3 review body was read in full from Bus record `reczz28SlQjtgpho`, and its single blocker
  was **independently verified against the locked dependency source** before acceptance:
  `Cargo.lock` pins `serde_json` **1.0.150** with checksum
  `e8014e44b4736ed0538adeecded0fce2a272f22dc9578a7eb6b2d9993c74cfb9`; in that crate's
  `src/value/de.rs` the `Value` map visitor performs `values.insert(key, value)` in a loop, so a
  duplicate key overwrites. `raw_value` was likewise verified to exist as a non-default feature
  (`raw_value = []` in that crate's `Cargo.toml`) and `RawValue::get()` to return the original
  `&str`. The reviewer's finding and remedy are therefore confirmed, not taken on trust.
- `main` re-checked as `d32b20beed8c8b56a2449f00a24fe26288a55d48`.
- Still **not done**: no product build, `cargo test`, clippy, e2e, Sonar or running node; no
  production or test code modified. Enabling serde_json's `raw_value` feature is a real Cargo
  change that implementation must make and that test 42's compile acceptance must cover.
- The three standing boundaries at the top of this revision remain in force.

================ END OF REVISION r4 — the text below is revisions r3, r2 then r1, retained as history. None is approved; where they differ from r4, r4 governs. ================

## RETAINED HISTORY — REVISIONS r3, r2 THEN r1 (superseded)


================ REVISION r3 — CURRENT REVISION (supersedes r2 and r1) ================

Design revision: r3
Revised: 2026-10-28
Repository: https://gitlab.okg.com/github/xlayer-reth   Branch: main (implementation lands on a NEW branch cut from it)
Inspected base SHA: d32b20beed8c8b56a2449f00a24fe26288a55d48
Local spec path: xlayer-reth/docs/superpowers/specs/2026-10-28-rcs-filter-direct-delegatecall-consumer-design.md
Jira Epic: https://okcoin.atlassian.net/browse/XLOP-1239
Source design: https://okg-block.sg.larksuite.com/docx/ODE0deAHnoyEfcxixyIlkyl4gWf (modified 2026-10-28T06:44:09Z)
Unit: medivac-directcallconsumer-starport-rcs   Building: starport-rcs
Correlation id: direct-delegatecall-units-20261028-001
Protocol version: stays 1. Convergence target: O2.

WHY r3 EXISTS: revision r2 received CHANGES_REQUESTED from `starport-rcs`
(`starport-rcs-directcallconsumer-spec-r2-changes-20261028`, record `reczz28Sl2W46ZVp`, reply to
`medivac-directcallconsumer-starport-rcs-stage1.0-specreview-r2`). That decision **closed most of
the r1 items at the design level** and left exactly **two lifecycle / validation-order
contradictions**, plus **two interface details the reviewer settled directly**. r3 is focused on
those four things and nothing else.

CLOSED BY THE r2 DECISION AND NOT REOPENED: O2 / v1; root-default-reject with explicit
exceptions, the pre-fold duplicate-alias visitor, and the explicit-null / empty-alias / wrong-type
validation direction; the EIP-7702 delegation-state + executed-code-identity predicate and tests
19b / 28 / 29; the acyclic `xlayer-rcs-capture` ownership and the budget-injection direction; the
optional `protocol_version` defaulting to 1 and accepting only 1; mixed rules requiring both
physical binding kinds while event-only keeps its existing semantics; the equivalence-regression
classification of tests 25 / 27 / 31. Real compilation and execution remain implementation
acceptance, not design claims.

ALSO EXPLICITLY ACCEPTED BY THE r2 DECISION — and therefore now a **standing obligation**: the
`known_allowed` trade-off is accepted as this round's *qualified* "semantics unchanged" contract
(execute / commit / `Allow` preserved; the capture result is diagnostic only; ordinary
non-`known_allowed` deposits are adjudicated by the new rules). **Its limitation — that a deposit
already listed in `known_allowed` which executes an unapproved implementation is NOT denied —
must be carried in the interface, in test 46, and in the final report.** It may never be
overwritten by an unqualified claim such as "all untrustworthy capture never returns Allow" or
"all deposits are protected". This acceptance authorises no new label and no change to the RCS
degradation policy, and does not represent an end-to-end security closure.

SCOPE OF r3 — the four changes:

  R1  §6.5.3 PARKING vs UNIFIED CLEANUP is a direct contradiction. Rewritten by separating
      ACTIVE WINDOW STATE from the PENDING RESULT.                            (§6.5.3, §6.5.6)
  R2  §7.2 / §6.2's claimed total priority is incompatible with eager serde decoding.
      Replaced by a real, executable two-phase contract.                      (§6.2.0, §7.2)
  R3  CT-F settled: RULE_LEGACY_TOLERATED_KEYS is EMPTY.                        (§6.1.1, §10)
  R4  V-9 narrowed to non-empty only; the ASCII grammar is withdrawn.                   (§6.2)

Plus the reviewer's implementation-verification notes, folded in where they change wording:
own-code evidence is the identity RECORDED AT PARENT FRAME ENTRY (§4.2.1), and `CaptureFaults`
must default to no faults without test construction altering production defaults (§6.5.5).

---

## R1 / §6.5.3 (replaces r2 §6.5.3) — Window state vs pending result

### 6.5.3.0 The contradiction r2 had
r2 said `finish_capture` PARKS the callstack result for a later `take`, and in the very next
bullet said every exit from `finish_capture` clears "...**and** the parked slot". Implemented
strictly to the second clause, the first `take` after a **successful** `finish` would also return
`ResultAlreadyTaken`, and **a non-empty observation set could never reach screening at all**. The
reviewer is right; this is a defect in r2, not a wording nit.

### 6.5.3.1 Two distinct state groups (normative)
The inspector holds two groups that are cleared on **different** schedules. Conflating them is
what produced the r2 contradiction.

| Group | Members | Cleared by |
|---|---|---|
| **ACTIVE WINDOW STATE** | `active`, `queue`, `checkpoints`, `frames`, `create_recipients`, `observations`, `observation_count`, `callstack_overflowed`, `frames_unbalanced`, `own_code_indeterminate`, the native sticky flags | `start_capture`, every exit of `finish_capture`, `abort_capture` |
| **PENDING RESULT** | `parked: Option<Observations>` | `start_capture`, `abort_capture`, and a `take` (move-consume). **NOT** by `finish_capture`. |

```rust
fn finish_capture(&mut self, result_logs: &[Log]) -> CaptureOutcome {
    // 1. Compute BOTH results from the still-populated active state.
    let callstack = self.compute_callstack_result();      // moves `observations` out
    let native    = self.compute_native_outcome(result_logs);
    // 2. Clear ACTIVE WINDOW STATE and disarm — on EVERY path, including the error paths.
    self.clear_active_window();                            // does NOT touch `parked`
    // 3. Install the pending result LAST, so no exit path can erase it.
    self.parked = Some(callstack);
    native
}

fn take_direct_delegatecalls(&mut self) -> Observations {
    self.parked.take()
        .unwrap_or(Observations::Unavailable(ObservationsUnavailable::ResultAlreadyTaken))
}

fn start_capture(&mut self) { self.clear_active_window(); self.parked = None; self.active = true; }
fn abort_capture(&mut self) { self.clear_active_window(); self.parked = None; }
```

Ordering is load-bearing: the parked result is installed **after** `clear_active_window()`, so
"cleared on every exit path" and "the result survives for one take" are no longer in conflict.

### 6.5.3.2 The edge cases r2 left undefined (normative)

| Situation | Behaviour |
|---|---|
| **Successful `finish` → first `take`** | returns `Available { stamp, items }` with the **exact** stamp and the full item list. This is the success channel and it must be proven by a test. |
| **Second `take`** | `Unavailable(ResultAlreadyTaken)`. The first `take` move-consumed it. |
| **`take` with no preceding `finish`** | `Unavailable(ResultAlreadyTaken)` — the slot is empty. |
| **Repeated `finish`** (`finish` called twice without an intervening `start`) | The second `finish` runs on a cleared, disarmed window: it parks `Unavailable(NotCaptured)` (overwriting the first result) and returns `CaptureOutcome::Passthrough`. Overwriting is deliberate and safe — a stale result must never be served against a new call — and the caller contract is one `finish` per `start`. |
| **`finish` while never armed** | Same as repeated `finish`: parks `Unavailable(NotCaptured)`, returns `Passthrough`. An enabled filter receiving `Passthrough` already fails closed, so this cannot become an `allow`. |
| **Native cross-check / budget failure** | The **native** outcome is returned on the `CaptureOutcome` channel exactly as before and remains **authoritative**. What is parked is the *callstack* result, computed independently. It can **never** soften a native `Deny`: the caller uses `fail_closed_screen` on the native outcome **first**, and only reaches rule evaluation when the native outcome is `Complete`. A parked `Available` alongside a native failure is therefore never consulted. |
| **New window opened without taking** | `start_capture` clears the stale `parked`, so the previous window's result can never be attributed to the new transaction. |
| **`abort_capture`** | clears both groups, so an aborted transaction leaves nothing behind. |

### 6.5.3.3 `ObservationsUnavailable` gains one variant

`ResultAlreadyTaken` joins `NotCaptured`, `IncompleteFrames`, `BudgetExceeded`,
`SnapshotMismatch`, `OwnCodeIndeterminate` and `EngineError`. Like the others it is fail-closed
input: if a rule declares an alias and the result is `ResultAlreadyTaken`, the transaction is
denied (§6.7) rather than treated as "nothing matched".

### 6.5.6 Lifecycle acceptance (replaces r2 tests 37–39)

r2's tests 37–39 only covered *no finish*, *repeated take* and *abort* — they could all pass while
the **success channel was broken**, which is exactly the r2 defect. Replaced by real sequences:

| # | Sequence | Expected |
|---|---|---|
| **37** | `configure(aliases)` → `start` → execute a tx producing ≥ 2 observations → `finish` → **first** `take` | `Available { stamp, items }`: the stamp equals the configured snapshot stamp **field by field**, and `items` equals the expected observations **in execution order**. This is the test that would have caught the r2 defect. |
| **38** | …then a **second** `take` | `Unavailable(ResultAlreadyTaken)` |
| **39** | `take` with no preceding `finish` | `Unavailable(ResultAlreadyTaken)` |
| **48** | `finish` whose **native** path fails (budget or real-log mismatch) while callstack observations exist | `finish` returns the native `LimitExceeded` / `InvariantViolation`; the caller fails closed **without** consulting the parked result; the native `Deny` is **not** softened |
| **49** | `finish` → `finish` again without `start` | second returns `Passthrough` and parks `Unavailable(NotCaptured)`; a following `take` yields `NotCaptured` |
| **50** | `finish` → (no `take`) → `start` → new tx with **no** observations → `finish` → `take` | `Available` with an **empty** item list for the new window — never the previous window's items |
| **51** | `start` → observations → `abort` → `take` | `Unavailable(ResultAlreadyTaken)`; and a subsequent `start`/`finish`/`take` cycle works normally |
| **52** | Every `finish` error exit (each `CaptureOutcome` variant) | active window state fully cleared and disarmed in all cases, asserted on the inspector's own observable state |

---

## R2 / §6.2.0 and §7.2 — A real, executable validation-order contract

### 6.2.0 The contradiction r2 had
r2 promised the total order "unknown top-level key → `protocol_version` → V-0…V-11", and promised
to name **every** offending rule id and reason in one response. Verified against the base SHA,
that is **not implementable as stated**:

- `AuditTransactionsRequest.rules` is `Vec<RawRule>` (`crates/rpc/src/audit/rpc.rs`) and
  `RulesResponse.rules` is `Vec<crate::rules::RawRule>` (`crates/rcs-filter/src/client.rs`,
  decoded eagerly by `decode_json`). So **serde finishes decoding every rule before the handler
  body runs at all.**
- Therefore `RawRule`'s `try_from`, `RawCallstack`'s `deny_unknown_fields`, the duplicate-alias
  visitor and every JSON type check (V-7, V-10, V-11) fire **before** the handler can look at the
  top-level unknown map or at `protocol_version`.
- A request carrying an unknown top-level key **and** `protocol_version: 2` **and** a malformed
  `callstack` type would surface a nested serde error first.
- Nothing can aggregate per-rule `(id, reason)` across all rules, because the first serde failure
  aborts the whole decode.
- The same inversion exists *inside* a rule: V-7 / V-10 / V-11 are serde-level and precede V-0's
  semantic key check.

### 6.2.1 Chosen contract: **two-phase, raw-first** (normative)

The promised priority is worth keeping — it is what makes a misspelled safety field diagnosable —
so the decode is restructured rather than the promise weakened. Both entry points use the same
two phases.

**Phase A — envelope, untyped.** The request / response is deserialized into a shape whose rules
are **not yet typed**:

```rust
#[derive(Deserialize)]
struct AuditRequestEnvelope {
    #[serde(default = "default_protocol_version")] protocol_version: u32,
    rules: Vec<serde_json::Value>,          // UNTYPED on purpose
    txs: Vec<DepositTxRequest>,
    #[serde(default)] known_allowed: Vec<String>,
    #[serde(flatten)] unknown: BTreeMap<String, serde_json::Value>,
}
```

Phase A can only fail on envelope-shaped problems. In Phase A order:
1. `unknown` non-empty ⇒ `-32602`, naming every unknown top-level key.
2. `protocol_version != 1` ⇒ `-32602`, naming the received value and the supported set `[1]`.

**Phase B — per-rule, typed, fully aggregated.** Only once Phase A passes, each
`serde_json::Value` is converted independently, so **one bad rule cannot hide the others**:

```rust
for (i, raw) in envelope.rules.iter().enumerate() {
    let id = raw.get("id").and_then(|v| v.as_str()).map(str::to_owned);     // best effort
    match compile_one(raw) { Ok(r) => ok.push(r), Err(reason) => rejected.push((id, i, reason)) }
}
```

Per-rule order inside `compile_one`, now fully specified and implementable:
1. **V-0** — enumerate the raw JSON object's keys and reject any outside
   `RULE_LEGACY_TOLERATED_KEYS` that is not a modelled field. This runs on the **raw value**, so
   it genuinely precedes the typed decode.
2. **V-7 / V-10 / V-11** — `serde_json::from_value::<RawRule>`: unknown key inside `callstack`,
   duplicate alias (pre-fold visitor), and wrong JSON types.
3. **V-1 … V-6, V-8, V-9** — semantic validation, in the numbered order.

`rejected` is reported **in full**: every rejected rule contributes its `id` (or, when `id` is
absent or not a string, its **0-based request index**, reported as such rather than guessed).
Non-empty `rejected` ⇒ `-32602` listing all of them.

**On both phases: `-32602`, and nothing is executed, installed or committed.** On the `/rules`
hot-reload path the equivalent failure surfaces as a `FilterError` on the existing `get_rules()`
decode path, and last-known-good rules stay in force.

### 6.2.2 What is explicitly *not* promised
There is no claim that a malformed JSON document (unbalanced braces, a non-object rule element,
a non-array `rules`) is diagnosed in this order. A document that cannot be parsed into
`AuditRequestEnvelope` at all fails with the serde error, still as `-32602`, still with nothing
executed. Phase A's ordering guarantee begins once the envelope parses.

### 6.2.3 Acceptance (new)

| # | Scenario | Expected |
|---|---|---|
| **53** | One request carrying **all three** at once: an unknown top-level key, `protocol_version: 2`, and a rule with a malformed `callstack` type | `-32602` citing the **unknown top-level key** (Phase A step 1) — proving the order is real and not an artifact of decode order. Nothing executed or committed. |
| **54** | `protocol_version: 2` plus a malformed rule, no unknown top-level key | `-32602` citing the **version** (Phase A step 2), not the rule |
| **55** | Valid envelope; **three** rules each failing differently (V-0 misspelled root key, V-10 duplicate alias, V-4 uppercase address) | one `-32602` listing **all three**, each with its id and reason — the aggregate promise, now real |
| **56** | The same payloads as 53 and 55 but with the JSON object keys **reordered** (and `rules` elements reordered) | identical verdicts and identical cited reasons — the order is not a function of input key order |
| **57** | A rule whose `id` is missing or not a string, rejected by V-4 | reported by its **0-based request index**, explicitly labelled as an index, never a fabricated id |
| **58** | A rule failing **both** V-0 and V-10 | V-0 is reported (raw-key check precedes the typed decode) |

Tests 53–58 run at **both** entry points.

---

## R3 / §6.1.1 and §10 — CT-F settled: the legacy allowlist is EMPTY

The reviewer settled this directly, so it is no longer an open item:

```rust
/// EMPTY, deliberately. Every wire key of the producer's typed Rule — including the new
/// `callstack` — is modelled by `RawRuleWire`, and neither source design identifies any
/// additional rule-root field that could be safely ignored. The producer tolerating
/// case variations of `id` / `action` ON READ does not mean it EMITS those keys to this node.
/// Consequence: EVERY unmodelled rule-root key rejects the rule (V-0).
/// Adding an exception requires per-key evidence and a fresh design review. This list is NOT
/// left open "in case there is a production extension".
const RULE_LEGACY_TOLERATED_KEYS: &[(&str, &str)] = &[];
```

The §13.2 "left to the implementer" entry for this list is **removed** — it is decided, not
deferred. CT-F is correspondingly narrowed from "agree the list" to: **confirm this explicit
reject-all contract, and the envelope key list, with the new producer.** That confirmation has
been sent (`medivac-directcallconsumer-starport-rcs-producer-schema-coord-r1`); a producer reply
that contradicts it is a design-affecting change and returns here, not a reason to widen the list
unilaterally.

---

## R4 / §6.2 — V-9 narrowed to non-empty

r2's V-9 required a "non-empty printable ASCII identifier". The reviewer is correct that the ASCII
grammar is **not** a constraint of this source design, which only requires a custom, non-empty
alias. There is no necessity or compatibility evidence for a narrower grammar, so inventing one
would be exactly the kind of unilateral tightening this design must avoid.

**V-9 (revised):** reject only an **empty** alias name —
`"direct_delegatecalls declares an empty alias name"`.

A non-empty alias that is not ASCII is **accepted**, and in particular must **never** be reported
using the empty-alias message. If a narrower grammar is ever wanted it must be frozen on **both**
sides with stated necessity and compatibility evidence, which is a separate change.

Test 34 is narrowed accordingly, and gains a positive case:

| # | Scenario | Expected |
|---|---|---|
| **34** | Empty alias name `""` | rejected with V-9's reason, both entry points |
| **34b** | A non-empty non-ASCII alias (e.g. `"代理1"`) used consistently in `callstack` and in `condition`'s `var` | **accepted**, binds and evaluates normally; no V-9 error and no empty-alias message |

---

## R-note / §4.2.1 — Own-code evidence is captured at parent frame ENTRY (normative)

The reviewer's implementation note is a real correctness point, not a restatement: a transaction
can change `A`'s account between the parent frame's entry and the child `DELEGATECALL` (that is
precisely the same-transaction upgrade this design exists to catch). So:

- Both own-code signals for frame `p` are **recorded when `p`'s interpreter is initialised**
  (`initialize_interp`) and **stored on `p`'s `FrameRecord`**:
  `executed_code_hash` (from `bytecode_slice()`) and `own_code: OwnCode::{Yes, No, Indeterminate}`.
- At the child `call` hook the staging decision reads **only** `p.own_code` and
  `p.executed_code_hash` from that record. It **must not** re-read the account, because the
  account state may already have changed.
- `FrameRecord` therefore gains `own_code: OwnCode`, and §4.2's table is evaluated **once per
  frame at entry**, not per child call.

This also makes the delegate-to-same-address and re-entry cases (§4.3) deterministic: each frame
carries its own entry-time evidence.

| # | Scenario | Expected |
|---|---|---|
| **59** | Within one transaction: enter `A`'s frame (own code), then **upgrade/change `A`'s account** mid-frame, then `A` `DELEGATECALL`s `C` | the observation uses `A`'s **entry-time** `own_code` and `contextCodeHash`; the mid-frame account change does not retroactively alter attribution |

`CaptureFaults` is likewise constrained: it must default to **no faults**, and constructing it in
a test must not change any production default. Test 42 asserts the default-constructed inspector
is behaviourally identical to the production one.

---

## r3 evidence boundary

- The r2 review body was read in full from Bus record `reczz28Sl2W46ZVp`, and **both** technical
  findings were independently verified against the real tree before acceptance:
  - R1 is a self-contradiction inside r2's own §6.5.3 text — confirmed by reading it back.
  - R2 confirmed in code: `AuditTransactionsRequest.rules: Vec<RawRule>`
    (`crates/rpc/src/audit/rpc.rs`), `RulesResponse.rules: Vec<crate::rules::RawRule>` and
    `get_rules()` → `decode_json` (`crates/rcs-filter/src/client.rs`). Serde therefore completes
    the typed decode of every rule before any handler check can run, at **both** entry points.
- `main` re-checked as `d32b20beed8c8b56a2449f00a24fe26288a55d48`.
- Still **not done**: no product build, `cargo test`, clippy, e2e, Sonar or running node; no
  production or test code modified. Joint acceptance with the producer remains
  INTEGRATION_PENDING. The `xlayer-rcs-capture` dependency graph remains a design claim whose
  compile acceptance is test 42.
- The `known_allowed` limitation recorded above is a **standing disclosure** that must appear in
  the interface table, test 46 and the final report.

================ END OF REVISION r3 — the text below is revision r2 then r1, retained as history. Neither is approved; where they differ from r3, r3 governs. ================

## RETAINED HISTORY — REVISIONS r2 THEN r1 (superseded)


================ REVISION r2 — CURRENT REVISION (supersedes r1) ================

## 0. Identity of this revision

| Field | Value |
|---|---|
| Design revision | **r2** |
| Repository | https://gitlab.okg.com/github/xlayer-reth |
| Branch (authorized scope) | `main` (implementation lands on a NEW branch cut from it) |
| Inspected base SHA | `d32b20beed8c8b56a2449f00a24fe26288a55d48` |
| Local spec path | `xlayer-reth/docs/superpowers/specs/2026-10-28-rcs-filter-direct-delegatecall-consumer-design.md` |
| Source design (Player-authorized) | https://okg-block.sg.larksuite.com/docx/ODE0deAHnoyEfcxixyIlkyl4gWf — modified `2026-10-28T06:44:09Z` |
| Jira Epic | XLOP-1239 — https://okcoin.atlassian.net/browse/XLOP-1239 |
| Unit / Building | `medivac-directcallconsumer-starport-rcs` / `starport-rcs` |
| Correlation id | `direct-delegatecall-units-20261028-001` |
| Protocol version | **stays 1** (no rule protocol bump) |
| Convergence target | **O2 — decided by the reviewing building** (§12) |

**WHY r2 EXISTS.** Revision r1 received `CHANGES_REQUESTED` from `starport-rcs` over the Terran
Bus (`starport-rcs-directcallconsumer-spec-r1-changes-20261028`, record `reczz28Skbf6IAfL`,
reply to `medivac-directcallconsumer-starport-rcs-stage1.0-specreview-r1`). That decision
**selected convergence option O2** and raised five technical groups. r2 is focused on exactly
those six items. Nothing the reviewer accepted is reopened.

**ACCEPTED IN r1 AND NOT REOPENED** (still to be really verified in implementation): record only
after a frame truly entered the interpreter; suppress candidates when a declared alias has no
observation; hash the actually-executed bytes; retain executed observations after a revert;
snapshot pinning with fail-closed on untrustworthy observations; the inert path; a separate
callstack capture budget plus the combined budget.

**SCOPE OF r2 — the six changes**

| # | Change | Section |
|---|---|---|
| D0 | O2 adopted as the convergence target; r1's open decision is closed. | §12 |
| D1 | Strict parsing boundary completed: root-level gate with an enumerated legacy allowlist, duplicate-alias rejection before map folding, empty-alias rejection, explicit-null/wrong-type rejection, negatives at **both** entry points. | §6.1, §6.2 |
| D2 | The own-code predicate is rebuilt on executed-code identity + delegation state, because r1's address-only test is **wrong** under EIP-7702. | §4, §6.5 |
| D3 | A real shared capture crate, dependency graph, trait, constructor/budget path and lifecycle contract — r1's §7.3 could not compile. | §6.5.2, §7.3 |
| D4 | The audit-RPC contradictions are frozen into one self-consistent contract, with the `known_allowed` conflict named rather than papered over. | §7.1, §7.2, §7.4 |
| D5 | Mixed event+direct candidate semantics now require **both** binding kinds, per source §4.2. | §6.4 |

---

## D0 / §12 (replaces r1 §12) — Convergence decision: O2, as decided

r1 presented O1/O2/O3 and recommended O1. **The reviewing building decided O2** and explicitly
did not approve O1 dual-schema coexistence, nor O3's wait-for-!79-then-converge. Recorded
rationale: the Player required two new units, manually stopped the two old units, and required
`protocol_version = 1`; the source document requires that `callstack` carry only
`direct_delegatecalls` and that the older structure be rejected. A historical design PASS was a
decision for its own scope and does not override the newest authorization.

**Binding consequences of O2 (normative):**

1. `callstack` admits **exactly one** field, `direct_delegatecalls`. The v2 selector form, and
   the variables `matched` / `callType` / `address` / `codeHash` / `implAddress` /
   `implCodeHash`, are **not** part of this design. `SUPPORTED_PROTOCOL_VERSIONS` stays `[1]`.
2. Implementation starts from the verified `main` baseline
   (`d32b20beed8c8b56a2449f00a24fe26288a55d48`) on a **new** branch.
3. The old units are not reused or resumed. The old `CT-11` / `known_allowed_kinds` contract is
   **not** imported (§7.4). MR !79 is **not** auto-closed, rewritten, or merged by this unit.
4. The new schema is **not** reinterpreted in order to keep old tests passing. Where an old test
   encoded the v2 selector contract, it is out of scope for this design rather than
   accommodated.
5. **Existing event and native-transfer behaviour must still be regression-protected.** That
   obligation is unchanged by O2 and is pinned by tests 25, 27 and 31.

No further Player approval is required for the existing identity or for this O2 direction.

---

## D1 / §6.1 (replaces r1 §6.1 and §6.1.1) — Wire schema and the strict parsing boundary

### 6.1.0 The defect r1 had

r1 §6.1.1 claimed a misspelled top-level `callStack` would be caught by V-1. **That is only true
when `event_abis` is empty.** Verified against the base SHA: `RawRule` derives `Deserialize`
*without* `deny_unknown_fields` (`crates/rcs-filter/src/rules/model.rs`), so a rule carrying a
valid `event_abis` **and** a misspelled `callStack` silently drops the misspelled key and loads
as a **weakened event-only rule** — exactly the unconstrained-version outcome this design exists
to prevent. r1's scoping argument is withdrawn.

Two further r1 gaps, both real:
- `BTreeMap<String, String>` **cannot** detect a duplicate alias key: serde folds the map and the
  last value silently wins. Duplicate keys must be rejected **before** the map folds.
- V-1…V-7 had no empty-alias check, and `Option<RawCallstack>` with `#[serde(default)]` accepts
  an explicit `"callstack": null` as "absent" rather than rejecting it.

### 6.1.1 Root-level gate (normative)

Unknown rule-level keys are **rejected by default**, with a single explicit, enumerated and
individually justified legacy allowlist. Unconditionally ignoring root unknown keys is forbidden;
so is unconditionally rejecting them, because that would break already-deployed rules.

```rust
/// Rule-level keys this build knowingly tolerates, each with a recorded reason. Anything not
/// modelled and not listed here rejects the rule. The list is DATA, reviewed on change — never a
/// catch-all.
const RULE_LEGACY_TOLERATED_KEYS: &[(&str, &str)] = &[
    // ("key", "why tolerating it is safe: it cannot weaken a rule's constraint")
];

#[derive(Debug, Clone, Deserialize)]
pub struct RawRuleWire {
    pub id: String,
    #[serde(default)] pub contract_address: Option<String>,
    #[serde(default)] pub origin: Option<String>,
    #[serde(default)] pub event_abis: BTreeMap<String, EventAbi>,
    #[serde(default = "default_audit_types")] pub audit_types: Vec<String>,
    pub condition: serde_json::Value,
    pub action: Action,
    #[serde(default)] pub audit_timeout_action: Option<TimeoutAction>,
    /// Present-vs-explicit-null is preserved: `None` = absent, `Some(None)` = explicit null
    /// (rejected by V-8), `Some(Some(_))` = a real object.
    #[serde(default, deserialize_with = "double_option")]
    pub callstack: Option<Option<RawCallstack>>,
    #[serde(flatten)]
    pub unknown: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(try_from = "RawRuleWire")]
pub struct RawRule { /* modelled fields only; `unknown` never survives */ }
```

`TryFrom<RawRuleWire> for RawRule` rejects with a reason **naming every offending key** when
`unknown` contains any key outside `RULE_LEGACY_TOLERATED_KEYS`. Because a rejected rule already
refuses the whole snapshot on the hot-reload path and the whole request on the audit path, a
misspelled safety field can no longer degrade a rule — it fails loudly.

The same `try_from` gate is applied to `RulesResponse` so an unknown **envelope** key is named
and surfaced on the existing `get_rules()` decode path as a `FilterError`, leaving
last-known-good rules in force. `RulesResponse`'s existing required `protocol_version` is
untouched.

### 6.1.2 The `callstack` object (normative)

```rust
/// Under protocol_version 1 the ONLY admissible field is `direct_delegatecalls`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawCallstack {
    /// Deserialized through a duplicate-rejecting visitor, NOT straight into a map.
    #[serde(deserialize_with = "alias_map_reject_duplicates")]
    pub direct_delegatecalls: BTreeMap<String, String>,
}

/// Rejects a duplicate alias key before the map folds, so a repeated alias can never be
/// silently collapsed to its last value.
fn alias_map_reject_duplicates<'de, D>(d: D) -> Result<BTreeMap<String, String>, D::Error>
where D: serde::Deserializer<'de> {
    // Visit entries one at a time; on an already-present key return
    // `de::Error::custom("direct_delegatecalls declares alias '<a>' more than once")`.
}
```

Wire example (unchanged from r1):

```json
{
  "id": "proxy-codehash-guard",
  "event_abis": {},
  "callstack": { "direct_delegatecalls": { "proxy1": "0x1111111111111111111111111111111111111111" } },
  "condition": { "!": { "in": [ { "var": "proxy1.delegateCodeHash" }, [ "0xbbbb…bbbb" ] ] } },
  "action": "deny"
}
```

### D1 / §6.2 (replaces r1 §6.2) — Validation order (normative)

| # | Rule | Rejection reason (verbatim) |
|---|---|---|
| V-0 | an unknown rule-level key outside the legacy allowlist | `"rule declares unknown field(s): {keys}"` |
| V-1 | `event_abis` is empty **and** `callstack` is absent | `"rule declares neither event_abis nor callstack"` |
| V-2 | `direct_delegatecalls` is present but empty | `"direct_delegatecalls is empty"` |
| V-3 | more than `MAX_DIRECT_DELEGATECALL_ALIASES` (= 8) aliases | `"direct_delegatecalls declares {n} aliases; at most 8 are allowed"` |
| V-4 | an alias value is not a lowercase, `0x`-prefixed, 20-byte hex string | `"direct_delegatecalls alias '{a}' is not a lowercase 0x 20-byte address"` |
| V-5 | an alias name collides with an `event_abis` key | `"direct_delegatecalls alias '{a}' collides with an event alias"` |
| V-6 | `action == audit` and `event_abis` is empty | `"direct-delegatecall-only rule cannot be an audit rule"` |
| V-7 | any key other than `direct_delegatecalls` inside `callstack` | serde: `"unknown field \`{k}\`, expected \`direct_delegatecalls\`"` |
| **V-8** | `callstack` is explicitly `null` | `"callstack is explicitly null; omit the field instead"` |
| **V-9** | an alias name is empty or is not a non-empty printable ASCII identifier | `"direct_delegatecalls declares an empty alias name"` |
| **V-10** | a duplicate alias key | `"direct_delegatecalls declares alias '{a}' more than once"` (serde, pre-fold) |
| **V-11** | `callstack` / `direct_delegatecalls` / an alias value has the wrong JSON type | serde type error naming the field |

V-4's check is on the **string form before parsing**, because `Address::from_str` accepts mixed
case: `s.len() == 42 && s.starts_with("0x") && s[2..].bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())`.
A checksummed address is a rejection, not a normalisation.

**V-1 changes a pre-existing behaviour**: the unconditional `"event_abis is empty"` rejection
becomes conditional. Test 20 pins that a rule declaring neither section is still rejected with
that original reason, so the change cannot weaken B1.

**Both entry points carry the identical gate.** `/rules` hot-reload and the
`xlayer_auditTransactions` request both go through `load_rules`, and the audit handler already
rejects the whole request with `-32602` when any rule is rejected. Tests 20–22 and 32–36 assert
each negative at **both** entry points.

---

## D2 / §4 (replaces r1 §4) — The own-code predicate, corrected for EIP-7702

### 4.0 Why r1 was wrong

r1 defined "the parent frame was executing A's own code" as
`p.target_address == A && p.bytecode_address == A`. **That is insufficient**, and the following
is verified in the locked dependencies:

| Fact | Location (locked version) |
|---|---|
| For a 7702-delegated account, call setup replaces the executed bytecode with the **delegate account's** code and hash: `bytecode = delegate_account.code…; code_hash = delegate_account.code_hash()`. | `revm-interpreter 37.0.3` `src/instructions/contract/call_helpers.rs` (the `// load delegate code if account is EIP-7702` branch) |
| Yet the frame still records `bytecode_address: to` — the original callee, **not** the delegate. | `revm-interpreter 37.0.3` `src/instructions/contract.rs` (`CallInputs { … bytecode_address: to, known_bytecode: (bytecode_hash, bytecode) … }`) |
| And the handler passes that address into the interpreter unchanged: `bytecode_address: Some(inputs.bytecode_address)`. | `revm-handler 20.0.3` `src/frame.rs` (`InputsImpl { … }`) |
| A 7702 delegation sets the account's own code to the **designator** and its `code_hash` to that designator's hash: `Bytecode::new_eip7702(address)`, `hash = bytecode.hash_slow()`. | `revm-context-interface 19.0.3` `src/journaled_state/account.rs` — `fn delegate` |
| `bytecode_slice()` returns `original_byte_slice()` — real bytes, not interpreter padding. (r1's hash direction is therefore sound and is retained.) | `revm-interpreter 37.0.3` `src/interpreter/ext_bytecode.rs` — `impl LegacyBytecode for ExtBytecode` |

**The counterexample.** Let `A` be selected **and** be a 7702-delegated account whose designator
points at `B`. A `CALL A` frame has `target_address == A` and `bytecode_address == A`, but the
code that actually runs is **B's**. If that code then `DELEGATECALL`s `C`, the child frame has
`scheme == DelegateCall` and a parent satisfying r1's address-only test — so **r1 would wrongly
attribute B's delegatecall to A's own code**. r1's test 19 only proved the outer `CALL` produces
no direct observation; it missed this child call entirely. Test 19b closes it.

### 4.1 Corrected predicate (normative)

Own-code is decided from **executed-code identity and delegation state**, never from addresses
alone, and never from a proxy storage slot or calldata.

For a newly entered frame `f` with parent frame `p`:

```
is_direct_delegatecall_of(A)  ⟺
      f.scheme == CallScheme::DelegateCall
   && p exists
   && p.target_address   == A
   && p.bytecode_address == A
   && p_executed_a_s_own_code                       // NEW — see 4.2
   && f actually entered the interpreter            // initialize_interp, C12
```

### 4.2 `p_executed_a_s_own_code` (normative)

Two independent facts are read, and they must **agree**:

1. **Delegation state.** `A`'s account code, read from the journal
   (`context.journal_ref().evm_state().get(&A)` — the same journal-read pattern the existing
   `create` hook already uses for the caller nonce). `A` executed its own code only if that code
   is present and **not** a 7702 designator: `!code.is_eip7702()` (`revm-bytecode 11.0.1`
   exposes `is_eip7702()` and `eip7702_address()`).
2. **Executed-code identity.** `keccak256(p_interpreter.bytecode.bytecode_slice())` compared to
   `A`'s account `code_hash`. They are equal for a normal contract, and **unequal** for a
   delegated account, because the account hash is the designator's hash while the executed bytes
   are the delegate's.

Combination rule, fail-closed by construction:

| Signal 1 (not delegated) | Signal 2 (hashes equal) | Result |
|---|---|---|
| true | true | own code — **observe** |
| false | false | delegated — **do not observe** (not an error; it is a correct exclusion) |
| true | false | **disagreement ⇒ `ObservationsUnavailable::OwnCodeIndeterminate`**, fail closed |
| false | true | **disagreement ⇒ `OwnCodeIndeterminate`**, fail closed |
| account code unavailable (not loaded) | — | **`OwnCodeIndeterminate`**, fail closed |

A disagreement is never silently resolved in either direction: a wrong "observe" would
misattribute code, and a wrong "skip" would silently drop a real observation. Both are
`A2`/`A3` violations, so the transaction fails closed instead.

`p.executed_code_hash` is already recorded at `initialize_interp` (§6.5), so signal 2 needs no
extra hashing; signal 1 is one journal map lookup, performed **only** when
`p.target_address ∈ aliases` — so the inert path (§6.9) is unaffected.

### 4.3 Which frame identity decides, and the re-entry edges (normative)

The deciding identity is always **the parent frame `p` of the `DELEGATECALL` frame**, as recorded
on entry to `p`, never the current top of stack at observation time and never the transaction's
`to`.

- **Nesting.** If `p` is itself a delegate frame then `p.bytecode_address != p.target_address`
  and `p` is excluded — an implementation's own nested delegatecall is never attributed to the
  proxy. (Test 6.)
- **Delegate-to-same-address.** `A` `DELEGATECALL`s `A`. The child frame has
  `target_address == bytecode_address == A`, so the child itself *looks* like an own-code frame.
  That is **correct and intended**: the child is a real, observable direct delegatecall of `A`
  (`delegate_address == A`), and because the child also satisfies own-code, a *further*
  `DELEGATECALL` from inside it is **also** attributed to `A`. This is faithful — `A`'s own code
  is what issued both. Test 28 pins it, including the emitted field values.
- **Re-entry.** `A → X → A`: the second `A` frame is a fresh frame with its own record and its
  own `initialize_interp` hash, so a `DELEGATECALL` from it is observed independently. Two
  observations with identical field values are both kept — they are distinct execution facts.
  Test 29.
- **`CALLCODE`** is excluded structurally, by `CallScheme` variant, not by address comparison.
  Test 24.
- **7702-delegated `CALL`** to `A` produces no direct observation for `A` (test 19) **and** no
  observation for any `DELEGATECALL` the delegate's code makes (test 19b).

---

## D3 / §6.5.2 and §7.3 — The shared capture crate (new)

### 6.5.2 The defect r1 had, and the crate layout (normative)

Verified at the base SHA: `crates/builder/src/flashblocks/mod.rs` declares **`mod rcs_capture;`**
— private, not `pub` — and `crates/rpc/Cargo.toml` depends on `rcs-filter` but **not** on
`xlayer-builder`. r1 §7.3 constructed `RcsInspector` from the RPC crate, which **cannot
compile**. Duplicating the capture code is also forbidden (anti-goal A4 / structural equality).

New crate **`xlayer-rcs-capture`**, giving an acyclic graph:

```
rcs-filter            (NO revm; owns the observation data types, §6.3)
      ▲
      │
xlayer-rcs-capture    (revm + rcs-filter; owns RcsInspector + the capture trait)
      ▲            ▲
      │            │
xlayer-builder   xlayer-rpc
```

- `rcs-filter` keeps its no-EVM-dependency invariant: it owns
  `DirectDelegatecallObservation`, `SnapshotStamp`, `ObservationsUnavailable`, `Observations`
  (plain primitives only).
- `xlayer-rcs-capture` owns `RcsInspector`, `CaptureMode`, `DirectDelegatecallConfig`,
  `RcsCaptureControl`, `CaptureOutcome` and `CaptureInvariantError`. It is the **only** place
  frame logic exists.
- `xlayer-builder` moves `flashblocks::rcs_capture` into the new crate and re-exports nothing
  private; `xlayer-rpc` gains a `xlayer-rcs-capture` dependency.

### 6.5.3 Lifecycle contract (normative)

```rust
pub trait RcsCaptureControl {
    /// Installs the alias configuration for the NEXT window. `None` = inert.
    /// Called per candidate, immediately before its simulation (§6.5.1).
    fn configure_direct_delegatecalls(&mut self, config: Option<DirectDelegatecallConfig>);

    /// Resets all per-window state and arms the inspector.
    fn start_capture(&mut self);

    /// Ends the window, performs the native-log cross-check in `NativeAndCallstack` mode,
    /// disarms, and PARKS the callstack result for a subsequent `take`.
    fn finish_capture(&mut self, result_logs: &[Log]) -> CaptureOutcome;

    /// Removes and returns the parked callstack result. Idempotent in the sense that a second
    /// call returns `Unavailable(ResultAlreadyTaken)` — never a stale clone and never a panic.
    fn take_direct_delegatecalls(&mut self) -> Observations;

    /// Aborts the window (transact error / early exit), clearing everything INCLUDING any
    /// parked result, and disarms.
    fn abort_capture(&mut self);
}
```

Pinned contract points, each a test:
- **Parking.** `finish_capture` computes the callstack outcome and stores it in
  `parked: Option<Observations>`; the caller then `take`s it. A `take` without a preceding
  `finish` returns `Unavailable(ResultAlreadyTaken)`. Test 37.
- **Repeated take.** The second `take` returns `Unavailable(ResultAlreadyTaken)`; the parked slot
  is cleared by the first. Test 38.
- **Unified cleanup.** Every exit from `finish_capture` and `abort_capture` clears queue,
  checkpoints, frames, observations, counters, sticky flags **and** the parked slot, so nothing
  leaks into the next window. An early return on the error path must not skip this. Test 39.
- **Mode behaviour.** `NativeAndCallstack` is bit-for-bit today's behaviour and stays the mode
  the flashblocks candidate loop uses. `CallstackOnly` makes `observe_native_transfer`,
  `record_real_log` and `push_checkpoint` genuine no-ops, keeps the real-log queue empty, leaves
  `observed_count`/`overflowed` untouched, and makes `finish_capture` skip both the native-budget
  check and the real-log cross-check. The `NoOpInspector` impl keeps returning `Passthrough` and
  `Unavailable(NotCaptured)`. Tests 30, 31.
- **The native log channel and native budget contracts are unchanged on every path.** Test 31.

### 6.5.4 Budget and construction path (normative)

`XlayerAuditRpc<T>` carries no configuration today (`{ backend: Arc<T> }`, constructed in
`bin/node/src/main.rs` with only `backend`), so the budget is threaded explicitly:

```rust
pub struct AuditCaptureConfig { pub max_callstack_observations_per_tx: usize }
pub struct XlayerAuditRpc<T> { pub backend: Arc<T>, pub capture: AuditCaptureConfig }
```

Path: `--rcs-filter.max-callstack-observations-per-tx` → `RcsFilterArgs` →
`AuditCaptureConfig` → `XlayerAuditRpc` at its single registration site. Semantics: `0` is
**rejected at startup** (`FilterConfig::validate` already requires `>= 1` for its budgets, and
this budget joins that check), so an accidental zero can never silently make the first
observation overflow. The audit RPC is **not** gated by `rcs-filter.enabled` — it adjudicates the
rules carried in the request, not the node's installed snapshot — and only the budget *value* is
read from that argument group. Tests 40, 41.

### 6.5.5 Test seams across a crate boundary (normative)

A crate compiled as a dependency does **not** enable its own `cfg(test)`, and `pub(crate)` does
not cross a crate boundary. So every seam the consuming crates' tests need is a **real API**, not
a `cfg(test)` item: fault injection is expressed as an explicit constructor/config input
(a `CaptureFaults` value defaulting to "no faults"), reachable from both `xlayer-builder` and
`xlayer-rpc` tests. No test may depend on an invisible `cfg(test)` symbol. Test 42 is the compile
acceptance: both consuming crates build, and their **test** targets build, against the new
dependency graph.

---

## D4 / §7 — The audit RPC contract, frozen

### 7.1 (replaces r1 §7.1) — Request shape

r1 said "no new field" **and** demanded rejection of an unsupported `protocol_version`. Verified:
`AuditTransactionsRequest` is `{ rules, txs, known_allowed }` with **no** version field, and
`load_rules(1, 0, req.rules)` hard-codes 1. A field that cannot be read cannot be validated, so
r1 was self-contradictory. **Frozen resolution:** exactly **one** additive, backward-compatible
optional field is added, and nothing else.

```rust
pub struct AuditTransactionsRequest {
    #[serde(default = "default_protocol_version")]   // returns 1
    pub protocol_version: u32,
    pub rules: Vec<RawRule>,
    pub txs: Vec<DepositTxRequest>,
    #[serde(default)]
    pub known_allowed: Vec<String>,
    #[serde(flatten)]
    pub unknown: BTreeMap<String, serde_json::Value>,   // rejected by §7.2
}
```

- **Omitted ⇒ 1.** Every existing caller is byte-compatible and unaffected.
- **Only 1 is accepted.** Any other value, including 2, is `-32602`. This is deliberate under O2:
  the v2 selector contract is not part of this design, and no v2 label is imported.
- `RulesResponse` is **not** touched; its `protocol_version` already exists, is required, and
  describes `GET /rules`, a different message.
- No audit-specific callback or proxy rule format is added, and the response shape is unchanged.

### 7.2 (replaces r1 §7.2) — Request validation

In this order, installing and committing **nothing** on failure:
1. Unknown top-level request key ⇒ `-32602`, naming the keys.
2. `protocol_version != 1` ⇒ `-32602`, naming the received value and the supported set `[1]`.
3. Any rule rejected by V-0…V-11 ⇒ `-32602`, naming each rule id and reason (the existing
   `rules.rejected` all-or-nothing behaviour, retained).

### 7.4 (replaces r1 §7.4) — `known_allowed`: the conflict, named

Verified at the base SHA, in the per-deposit loop of `crates/rpc/src/audit/rpc.rs`: a deposit
whose `source_hash` is in `known_allowed` is executed, its state is **committed
unconditionally**, it is recorded as `Allow`, and `verdict_for` is **skipped** via `continue`.

That collides with r1's §7.3 ("every deposit is captured and classified") and §7.4 ("a capture
failure never returns `Allow`"). The collision is structural, and this design **states it rather
than resolving it silently**:

- The node **cannot** distinguish an ordinary `Original` (a deposit RCS already resolved to
  Allow) from a `SyntheticBan` replay prefix. `known_allowed` is a flat `Vec<String>` of
  `source_hash` values with **no kind label**, and inferring the kind from `to`, calldata or
  position would be a guess.
- The two classes have **opposite safe defaults**: deny-priority is safe for an `Original` and
  unsafe for a `SyntheticBan`; committing is safe for a `SyntheticBan` and unsafe for an
  `Original`. There is therefore no safe guess.
- O2 forbids importing the old `known_allowed_kinds` / CT-11 label, and the task constraint is
  that **`known_allowed` semantics stay unchanged**.

**Frozen contract for this round (normative).** Both constraints are honoured, and the residual
risk is disclosed instead of hidden:

| Deposit class | Execute | Capture armed | Verdict | State | Callstack result |
|---|---|---|---|---|---|
| not in `known_allowed` | yes | yes | §6.4 / §6.7 semantics; `malformed` non-verdict on untrustworthy observations, **never `allow`** | committed only on `allow` | authoritative |
| in `known_allowed` | yes | yes | **`allow`, unchanged** — `verdict_for` still skipped | **committed, unchanged** | **diagnostic only**: one bounded metric + one bounded log; no verdict depends on it |

- **Quota convergence is preserved**: an already-allowed quota transaction does not re-enter
  audit across rounds, byte-identical to the base SHA.
- **The synthetic prefix contract is preserved**: a prefix is never silently absent while the
  caller believes it exists.
- **§7.4's "never `allow`" is explicitly scoped** to deposits not in `known_allowed`.
- **Disclosed residual risk, stated plainly:** in this round a `known_allowed` deposit that
  executes unapproved implementation code is **not** denied. This is the direct cost of keeping
  `known_allowed` semantics unchanged without a producer-supplied class label. It is a known
  limitation of this revision, not an oversight.
- **The alternative contract, offered but not adopted here:** make a callstack objection
  deny-authoritative for `Original` entries only. That requires a producer-supplied, explicitly
  labelled class on the request, which O2 defers; it needs its own decision and a coordinated
  producer change, and this design does not take it unilaterally.

Capture being armed for `known_allowed` deposits costs nothing extra on the inert path and keeps
the ordered-state and diagnostic story coherent; because no verdict reads it, it cannot change
behaviour.

### 7.3 (amended) — Ordered simulation with real capture

Unchanged from r1 except that the inspector now comes from `xlayer-rcs-capture` (§6.5.2) and the
`known_allowed` rows follow the table above:

1. Compile the request's rules once; derive the request's own `SnapshotStamp` as
   `(protocol_version = 1, content_version = 0, alias_set_digest)` — matching
   `load_rules(1, 0, …)`. The request does not accept a `content_version`.
2. If no rule declares an alias, install `None`: the inert path applies, **no** inspector frame
   work happens, and the response is byte-identical to the base SHA. Test 25.
3. For each deposit **in request order**: arm capture, `evm.transact`, `finish_capture`,
   `take_direct_delegatecalls`, then classify per §7.4.
4. Each deposit's observations belong to that deposit only — one window per transaction, each
   beginning with a full reset.
5. State commit is unchanged: `Allow` commits, `Deny` discards and continues, `Audit` discards
   and stops. **A prior deposit's committed state is therefore visible to every later deposit**,
   so a deposit that upgrades a proxy makes later deposits resolve the new, real delegate target
   and its real code hash. Test 16.

---

## D5 / §6.4 (replaces r1 §6.4) — Candidate semantics, including mixed rules

### 6.4.0 The defect r1 had

Source design §4.2 requires that a mixed event + direct-delegatecall rule have **both** binding
kinds present and satisfied. Verified at the base SHA, the matcher gives a missing event one
`vec![None]` candidate and still evaluates the rule
(`crates/rcs-filter/src/matching.rs`, the `candidates` construction in `evaluate_checked`).
r1 suppressed only a missing **alias** and left the event null candidate in place. So with a
direct observation present, no event, and a condition that only inspects `delegateCodeHash`, a
mixed `deny` would still fire — violating source §4.2.

### 6.4.1 Rule classes and their candidate rules (normative)

A rule is classified once, at compile time:

| Class | `event_abis` | `direct_delegatecalls` | Candidate rule |
|---|---|---|---|
| **event-only** | ≥ 1 | absent | **Unchanged from the base SHA.** A missing event contributes `vec![None]`; the rule is still evaluated. Emergency Deny-All semantics preserved exactly. |
| **direct-only** | empty | ≥ 1 | Every declared alias must have ≥ 1 observation; otherwise the rule is **not evaluated**. |
| **mixed** | ≥ 1 | ≥ 1 | Every declared alias must have ≥ 1 observation **and** every declared event must have ≥ 1 *physical* binding; otherwise the rule is **not evaluated**. |

Suppression means `continue` to the next rule: no binding is built, `condition` never runs, and
the rule contributes **zero** to the combined budget (§6.6) — not one.

Why the three classes differ rather than unifying on one rule:
- Null-binding an **alias** would make `{"!": {"in": [{"var": "a.delegateCodeHash"}, [...]]}}`
  evaluate **true** for every transaction that never touched the proxy, because
  `jsonlogic::value_eq` returns `false` when exactly one side is null — a production-wide false
  deny (A1).
- Changing **event-only** behaviour would alter deployed rule semantics and the Emergency
  Deny-All contract, which is out of scope and explicitly regression-protected.
- **Mixed** follows source §4.2 directly: both binding kinds must exist.

### 6.4.2 Binding and merge (unchanged from r1)

For each surviving rule the candidate product gains one dimension per declared alias alongside
the per-event dimensions, ordered by **execution order**; `visit_bindings` walks the full
event × alias product lazily; `build_bindings` inserts the four flat keys per alias plus the
existing event keys; action merge is unchanged, with any candidate satisfying a `deny` rule
short-circuiting. An `audit` action is reachable only for a mixed rule (V-6) and takes its
payload from the **event** side only.

### 6.4.3 Budget (unchanged from r1, restated for the mixed case)

```
rule_binding_count = Π(event candidate counts) × Π(alias observation counts)
```
accumulated across rules with the existing `checked_binding_count` / `checked_add` discipline;
an overflow in either dimension yields `BindingCountOverflow` / `BindingBudgetExceeded` and fails
closed. Cross-rule accumulation, overflow and the executed-then-revert interaction are pinned by
tests 43–45.

---

## 9. Acceptance tests (replaces r1 §9)

Tests 1–18 remain the source design's matrix one-to-one. r1's 19–26 are retained, with 19b and
25 corrected. 27–45 are new in r2.

| # | Scenario | Expected |
|---|---|---|
| 1 | Proxy direct-delegatecalls an **approved** implementation | `allow` |
| 2 | Proxy direct-delegatecalls an **unapproved** implementation | `deny` |
| 3 | Ordinary `CALL` to the proxy, no delegatecall | no observation; rule not evaluated; `allow` |
| 4 | Two allowed delegate hashes | both `allow` |
| 5 | One unallowed delegate hash | `deny` |
| 6 | Implementation performs its own nested delegatecall | no observation for that alias |
| 7 | Same tx upgrades to an unapproved impl, calls it, then **reverts** | `deny`; observation survives |
| 8 | Several delegatecalls, one unapproved | `deny` |
| 9 | One rule declares `proxy1`+`proxy2`; only `proxy1` delegatecalls | rule not evaluated → no match |
| 10 | An **unselected** third proxy delegatecalls | not recorded |
| 11 | Observation budget exceeded | `deny` (`BudgetExceeded`) |
| 12 | Frame stack inconsistent / capture unavailable | `deny` (`IncompleteFrames` / `NotCaptured`) |
| 13 | Snapshot changed, old observation presented | `deny` (`SnapshotMismatch`) |
| 14 | L1 deposit, implementation **approved** | audit `allow` |
| 15 | L1 deposit, implementation **unapproved** | audit `deny` |
| 16 | Earlier deposit upgrades the impl; later deposit calls it | later deposit judged against the **new** impl |
| 17 | Audit request with `protocol_version` 2 (or any value ≠ 1), or an unknown `callstack` field | `-32602` naming value/keys; never `allow`; nothing committed |
| 18 | direct-delegatecall-only `audit` rule | rule load failure (V-6) |
| 19 | 7702-delegated `CALL` to a selected address | **no** direct observation for it |
| **19b** | **A is selected AND 7702-delegated to B; `CALL A` runs B's code; B's code `DELEGATECALL`s C** | **no observation attributed to A** — this is the r1 defect (§4.0) |
| 20 | Rule declaring neither section | rejected with V-1's reason, at **both** entry points |
| 21 | Checksummed / uppercase alias address | rejected (V-4), both entry points |
| 22 | A 9th alias; alias colliding with an event alias | rejected (V-3 / V-5), both entry points |
| 23 | `DELEGATECALL` that never entered the interpreter (depth limit, precompile, empty code) | no observation |
| 24 | `CALLCODE` from a selected address | no observation |
| 25 | **Inert path regression:** no alias declared; event-only rule with a non-zero internal native transfer | verdict, produced block and audit response **identical to the base SHA**; no inspector frame work (construction-site observable). Classified as an **equivalence regression** — it is *not* required to fail on the baseline. |
| 26 | Same rule, same frames, both paths | §6 and §7 verdicts **equal** |
| **27** | Event-only rule with a missing event | **unchanged** base-SHA behaviour: null candidate, rule still evaluated (Emergency Deny-All preserved) |
| **28** | `A` `DELEGATECALL`s `A`, then that frame `DELEGATECALL`s `C` | both observed for `A`; exact field values asserted (§4.3) |
| **29** | Re-entry `A → X → A`, each `A` frame delegatecalling | two independent observations; identical field values both retained |
| **30** | `CallstackOnly` mode | `observed_count` stays 0; no native-budget outcome even with more native transfers than `max_native_transfers`; real-log queue empty |
| **31** | `NativeAndCallstack` on the §6 path | native behaviour incl. budget overflow **unchanged** from the base SHA |
| **32** | Valid `event_abis` **+ misspelled `callStack`** | rejected (V-0) naming the key — **not** loaded as a weakened event-only rule. Both entry points. |
| **33** | Duplicate alias key | rejected (V-10) pre-fold, both entry points |
| **34** | Empty alias name | rejected (V-9), both entry points |
| **35** | `"callstack": null` | rejected (V-8), both entry points |
| **36** | `callstack` / `direct_delegatecalls` / alias value of the wrong JSON type | serde type error naming the field, both entry points |
| **37** | `take_direct_delegatecalls` without a preceding `finish_capture` | `Unavailable(ResultAlreadyTaken)` |
| **38** | Two consecutive `take`s | second returns `Unavailable(ResultAlreadyTaken)` |
| **39** | `abort_capture` after a window with observations, then a new window | nothing leaks: no parked result, counters and sticky flags cleared |
| **40** | A non-default `--rcs-filter.max-callstack-observations-per-tx` | actually bounds the audit RPC (value injected, not hard-coded) |
| **41** | `rcs-filter.enabled = false` + a request declaring an alias | still adjudicated against real capture; budget `0` rejected at startup |
| **42** | **Compile acceptance** | `xlayer-builder` and `xlayer-rpc`, and both crates' **test** targets, build against the `xlayer-rcs-capture` graph; no seam relies on an invisible `cfg(test)` |
| **43** | Mixed rule, direct observation present but **no event**, condition inspects only `delegateCodeHash` | rule **not evaluated** → no `deny` (the r1 defect, §6.4.0) |
| **44** | Mixed rule, event present but **no alias observation**; and both present | suppressed / evaluated respectively; multiple event × alias combinations asserted |
| **45** | Combined budget: cross-rule accumulation, overflow in either dimension, and an executed-then-reverted observation inside the product | fails closed on overflow; the reverted-but-executed observation still participates |
| **46** | Audit `known_allowed` non-empty: ordinary entry and a synthetic replay prefix, across multiple RCS rounds | `allow` + state committed for both, **unchanged** from the base SHA; callstack result reported as a bounded diagnostic only; quota convergence preserved; a later non-`known_allowed` deposit still adjudicated on the correct base state |
| **47** | Own-code signal disagreement, or `A`'s account code unavailable | `OwnCodeIndeterminate` ⇒ fails closed; never silently observed or silently skipped |

Tests 11–13, 19b, 23, 32–36, 43 and 47 are the ones that most easily pass for the wrong reason;
each must be written to fail against the pre-change tree with a recorded failure count. Test 25
and test 27 and test 31 are **equivalence regressions** and are expected to pass on the baseline
by definition — they are not required to fail there.

---

## 10. Interfaces this design does not own (replaces r1 §10)

| Id | Obligation | Owner |
|---|---|---|
| CT-A | Alias values lowercase, `0x`-prefixed, 20 bytes; ≤ 8 per rule; no alias/event collision; **no duplicate alias key**; **no empty alias name** | producer |
| CT-B | `callstack` carries no key but `direct_delegatecalls`; no explicit `null`; correct JSON types | producer |
| CT-C | Never publish a direct-delegatecall-only `audit` rule; mixed `audit` takes its payload from the event side | producer |
| CT-D | RCS Force-Tx must **not** treat the audit RPC's non-verdict as `allow` | RCS Force-Tx |
| CT-E | Consumer deployed before the producer enables such rules (no capability negotiation) | release owner |
| **CT-F** | The exact rejection scope for unknown / misspelled / legacy rule-level keys must be agreed with the producer, so `RULE_LEGACY_TOLERATED_KEYS` (§6.1.1) matches what the producer may actually emit | producer + this unit |
| **CT-G** | Audit requests send `protocol_version` 1 or omit it; version refresh on the producer side is coordinated before any change | producer |

Joint acceptance with `medivac-directcallproducer-starport-rcs` is **INTEGRATION_PENDING**; both
sides must pin exact SHAs. INTEGRATION_PENDING is **not** completion. The existing RCS degraded
fail-open path provides **no** end-to-end blocking guarantee, so a non-`Allow` return from this
node must not be read as "already protected".

---

## 11. Release order (normative)

Unchanged from r1: ship the consumer supporting both paths; verify both parse
`callstack.direct_delegatecalls` under protocol 1; then deploy the producer; publish an
approved-only rule and verify `allow`; switch to an unapproved implementation and verify both
ordinary transactions and L1 deposits `deny`; verify no false positives for no-proxy, ordinary
`CALL` and nested delegatecall; then enable the production rule.

---

## 13. Evidence, limits and what is left to the implementer

### 13.1 Evidence boundary for r2
- The r1 review body was read in full from the Bus row `reczz28Skbf6IAfL` and every one of its
  five technical claims was **independently verified against the real code** before being
  accepted — none was taken on the reviewer's word:
  - D1: `RawRule`'s derive without `deny_unknown_fields`; the private `mod rcs_capture`.
  - D2: the four locked-dependency facts tabulated in §4.0, each read from the vendored crate
    source at the pinned version in `Cargo.lock`
    (`revm-interpreter 37.0.3`, `revm-handler 20.0.3`, `revm-context-interface 19.0.3`,
    `revm-bytecode 11.0.1`).
  - D3: `crates/rpc/Cargo.toml` has `rcs-filter` but no `xlayer-builder`.
  - D4: the `known_allowed` branch in the audit loop commits unconditionally, returns `Allow`
    and `continue`s past `verdict_for`.
  - D5: the `candidates` construction in `evaluate_checked` gives a missing event `vec![None]`.
- Source design read in full; repository grounded through a CodeGraph index of this checkout
  (7,370 files / 149,359 nodes / 475,461 edges) plus direct reads of the rule model, validation,
  jsonlogic, matcher, handle, config, metrics, the capture inspector's non-test regions, the
  builder candidate loop, and both audit modules.
- MR !79 state read from GitLab (open; head `3a6028eb13c14c4fc1dbc365fc964636dadf6bfe`; base
  `d32b20beed8c8b56a2449f00a24fe26288a55d48`). Sibling designs r3/r5 read from Lark; their
  canonical hashes are their own published declarations, not independently recomputed.
- Jira XLOP-1239 read back as an Epic in project XLOP with the `biz-feature` label.
- **Not done:** no product build, `cargo test`, clippy, e2e, Sonar or running node. No production
  or test code was modified in this stage. The §6.5.2 crate graph is a design claim; its compile
  acceptance is test 42, to be executed during implementation.

### 13.2 Left to the implementer
- The exact module split inside `xlayer-rcs-capture` and private helper names.
- Whether `alias_set_digest` is keccak over sorted address bytes or another stable 32-byte
  digest, provided it is order-independent and documented.
- `MAX_OPEN_FRAMES`'s exact value, provided it is ≥ the EVM call-depth limit and the exceeded
  branch fails closed.
- The concrete `CaptureFaults` shape, provided it is a real API input and not a `cfg(test)` item.
- The initial contents of `RULE_LEGACY_TOLERATED_KEYS`, which must be settled with the producer
  under CT-F before the gate is enabled; shipping it empty (reject all unknown keys) is
  acceptable only once CT-F confirms the producer emits none.

### 13.3 Non-authorisation
This is a code-level design for review. It authorises no merge, no deployment, no release and no
scope expansion beyond `xlayer-reth` under Epic XLOP-1239. A design PASS is not test success and
not an MR approval.

================ END OF REVISION r2 — the text below is revision r1, retained as history. r1 is NOT approved; where r1 and r2 differ, r2 governs. ================

## RETAINED HISTORY — REVISION r1 (superseded)


## 0. Identity of this revision

| Field | Value |
|---|---|
| Design revision | **r1** |
| Repository | https://gitlab.okg.com/github/xlayer-reth |
| Branch (authorized scope) | `main` |
| Inspected base SHA | `d32b20beed8c8b56a2449f00a24fe26288a55d48` |
| Local spec path | `xlayer-reth/docs/superpowers/specs/2026-10-28-rcs-filter-direct-delegatecall-consumer-design.md` |
| Source design (Player-authorized) | https://okg-block.sg.larksuite.com/docx/ODE0deAHnoyEfcxixyIlkyl4gWf — modified `2026-10-28T06:44:09Z` |
| Jira Epic | XLOP-1239 — https://okcoin.atlassian.net/browse/XLOP-1239 |
| Unit / Building | `medivac-directcallconsumer-starport-rcs` / `starport-rcs` |
| Correlation id | `direct-delegatecall-units-20261028-001` |
| Protocol version | **stays 1** (no rule protocol bump) |

This document is the complete normative specification. It is written to be implemented
directly: there is no separate executable plan in this Flow. Section 12 is the one item
that requires a decision from the reviewing building before implementation starts, because
it concerns an already-approved sibling design on the same branch.

---

## 1. Intent and why

RCS publishes risk-control rules. Today a rule can only reason about **event logs** a
transaction emitted. That cannot answer the question this change exists for: *which code
actually ran on behalf of a selected contract?*

A proxy's `tx.to`, its stored implementation pointer, and its calldata all fail to answer
it. A transaction can upgrade a proxy, call through it into unapproved implementation code,
and roll the pointer back — all inside one transaction. Reading storage before or after
execution sees an approved pointer both times; the unapproved code still ran.

This change lets a rule select a contract address and observe, from the **real execution
frames** of a simulated transaction, the contract's own direct `DELEGATECALL`: the execution
context and the delegate target, each with the runtime code hash that genuinely executed.
Expected hashes live in `condition`, so an unapproved implementation produces an observation
a rule can **deny** on instead of being silently ignored.

The same semantics must hold for L1 deposits adjudicated through `xlayer_auditTransactions`,
because a deposit is a consensus-mandated transaction path that today runs no frame capture
at all.

### Why now
`xlayer-rcs` is about to ship the matching Rule Producer (`medivac-directcallproducer-starport-rcs`).
The existing protocol has **no capability negotiation**, so every consumer must understand
`callstack.direct_delegatecalls` *before* RCS is allowed to publish such a rule. Consumer
first, producer second (§11).

---

## 2. Goals, non-goals, anti-goals

### 2.1 Goals
- G1 Parse `callstack.direct_delegatecalls` under the existing `protocol_version = 1`.
- G2 Capture a selected contract's **own direct** `DELEGATECALL` during simulation, from real
  execution frames.
- G3 Expose exactly four condition variables per alias: `contextAddress`, `contextCodeHash`,
  `delegateAddress`, `delegateCodeHash`.
- G4 Execute `allow` / `deny` on those variables, and `audit` only for mixed event+callstack rules.
- G5 **Fail closed** whenever an observation cannot be trusted.
- G6 Give the ordinary-transaction Filter path and `xlayer_auditTransactions` **one** schema,
  one snapshot discipline, one condition semantics and one fail-closed behaviour.
- G7 Preserve ordered L1 deposit state effects across a batch, so a prior deposit's upgrade is
  visible to a later deposit.

### 2.2 Non-goals
- Rule generation, merging or version management in the RCS API.
- Reading EIP-1967 / EIP-1167 / Beacon or any other proxy implementation slot.
- Inferring an implementation from storage, calldata, or upgrade events.
- Observing `CALL`, `STATICCALL` or `CALLCODE`.
- Observing an implementation's own nested `DELEGATECALL`.
- An action payload for a direct-delegatecall-only `audit` rule.

### 2.3 Anti-goals — this change has failed even if it "works"
- **A1** A rule with no matching direct delegatecall denies unrelated transactions.
  (A false positive on this dimension drops honest user traffic.)
- **A2** "Capture was unavailable" is reported as "capture ran and nothing matched".
  Those two must never collapse; one is fail-closed, the other is a clean no-match.
- **A3** A code hash is reported that was not the hash of the bytecode the EVM actually executed.
- **A4** The deposit/audit path and the ordinary path give different verdicts for the same rule
  against the same execution frames.
- **A5** A deployment carrying no direct-delegatecall rule pays per-frame tracking cost.

---

## 3. Constraints discovered in the current code

All line references below were read from the working tree at the inspected base SHA.

| # | Constraint | Evidence |
|---|---|---|
| C1 | `RawRule` has **no** `callstack` field, and derives `Deserialize` **without** `deny_unknown_fields`, so an unknown wire field is silently ignored today. | `crates/rcs-filter/src/rules/model.rs` — `pub struct RawRule` |
| C2 | `compile_rule_with_interner` rejects a rule whose `event_abis` is empty with `"event_abis is empty"`. A direct-delegatecall-only rule is therefore **impossible** today. | `crates/rcs-filter/src/rules/validation.rs` |
| C3 | `MAX_EVENTS_PER_RULE = 8`. | `crates/rcs-filter/src/rules/mod.rs` |
| C4 | `parse_opt_address` uses `Address::from_str`, which accepts any case and performs no lowercase check. | `crates/rcs-filter/src/rules/validation.rs` |
| C5 | JSONLogic resolves `var` against a **flat** `Bindings = HashMap<String, Value>`; an unknown name yields `Value::Null`. So `"proxy1.delegateCodeHash"` is a legal flat key, exactly like `"<event>.<param>"`. | `crates/rcs-filter/src/rules/jsonlogic.rs` — `eval_var` |
| C6 | `value_eq` returns `false` when exactly one side is `Null`. Therefore `{"!": {"in": [{"var": "a.x"}, [..]]}}` evaluates **true** when `a.x` is absent. A deny rule that null-binds an absent observation denies everything. This is the concrete reason §5.3 and §6.4 use *candidate suppression* rather than null binding. | `crates/rcs-filter/src/rules/jsonlogic.rs` — `value_eq` |
| C7 | The matcher builds, per rule, one candidate vector per declared event; an event with no physical match contributes `vec![None]`, so **the rule is still evaluated** with null bindings. Changing "no observation ⇒ no match" therefore requires a real change in candidate generation, not just a binding value. | `crates/rcs-filter/src/matching.rs` — `evaluate_checked`, `visit_bindings` |
| C8 | `ScreenInput` carries only `logs: &'a [Log]`. There is no channel for frame observations. | `crates/rcs-filter/src/handle.rs` — `pub struct ScreenInput<'a>` |
| C9 | `RcsInspector` already owns a capture window (`RcsCaptureControl::{start,finish,abort}_capture`) and a fail-closed outcome type `CaptureOutcome::{Passthrough, Complete, LimitExceeded, InvariantViolation}`. | `crates/builder/src/flashblocks/rcs_capture.rs` |
| C10 | The inspector is constructed **once per `execute_best_transactions`**, outside the per-transaction loop. An alias set captured there can go stale against a snapshot that hot-swaps mid-loop. | `crates/builder/src/flashblocks/context.rs` — `RcsInspector::new(filter.max_native_transfers_per_tx())` |
| C11 | `fail_closed_screen` maps every non-`Complete` outcome to `Screen::Deny` and records a `LocalDenyReason`. An enabled filter seeing `Passthrough` is treated as a bug and also denies. | `crates/builder/src/flashblocks/context.rs` |
| C12 | **`revm-inspector 21.0.3` `inspect_frame_init` is the authoritative execution boundary.** `frame_start` fires `call`; an early-failure path (precompile, empty code, depth limit, failed value transfer) returns a `FrameResult` and fires `frame_end`→`call_end` **without ever calling `initialize_interp`**. `initialize_interp` is invoked **only** when a real interpreter frame was created. | `revm-inspector-21.0.3/src/traits.rs` — `inspect_frame_init` |
| C13 | `PostExecCompositeInspector::initialize_interp` forwards to `self.inner`, so `RcsInspector` receives it through the composite wrapper. | `deps/optimism/rust/alloy-op-evm/src/post_exec/inspector.rs` |
| C14 | `op-revm`'s `type Frame = EthFrame<EthInterpreter>`, so `frame.eth_frame()` is `Some` and `initialize_interp` really fires on this node. | `deps/optimism/rust/op-revm/src/evm.rs` |
| C15 | `InputsTr` exposes `target_address()`, `bytecode_address() -> Option<&Address>` and `caller_address()`; `CallInputs.scheme` is `CallScheme::{Call, CallCode, DelegateCall, StaticCall}`. | `revm-interpreter-37.0.3/src/interpreter_types.rs`, `.../call_inputs.rs` |
| C16 | `InterpreterTypes::Bytecode: … + LegacyBytecode`, and `LegacyBytecode::bytecode_slice()` returns the frame's actual loaded bytecode. `keccak256(bytecode_slice())` is therefore the **executed** code hash and needs no extra trait bound. | `revm-interpreter-37.0.3/src/interpreter_types.rs` |
| C17 | `AccountInfo.code_hash` is **not** always the executed code hash: under EIP-7702 call setup resolves the delegate's code while `bytecode_address` still names the original `to`. The account field must not be used as the source for §6.3. | upstream EIP-7702 call setup; consistent with the approved sibling design's reviewer-verified finding |
| C18 | `xlayer_auditTransactions` builds its EVM with `evm_with_env` — **no inspector** — and classifies `result.logs()` only. The audit path cannot observe frames at all today. | `crates/rpc/src/audit/rpc.rs` |
| C19 | The audit handler already rejects the whole request with `-32602` when any rule fails validation (`rules.rejected` non-empty), so all-or-nothing rule loading already holds. | `crates/rpc/src/audit/rpc.rs` |
| C20 | The audit loop already executes deposits one at a time in order and commits state only for `Allow` (`commit_and_continue`), so ordered state visibility for allowed deposits already exists and must be preserved, not rebuilt. | `crates/rpc/src/audit/rpc.rs` |
| C21 | `verdict_for` builds a `ScreenInput` and calls the same `matching::try_evaluate`, so shared condition semantics already hold for events and must be extended, not duplicated. | `crates/rpc/src/audit/verdict.rs` |
| C22 | `SUPPORTED_PROTOCOL_VERSIONS = &[1]` and `FilterConfig` already validates `max_*_per_tx >= 1`. | `crates/rcs-filter/src/config.rs` |

---

## 4. The decisive semantic finding

> **A `DELEGATECALL` is a direct delegatecall of a selected address A if and only if the
> frame's parent frame was executing A's own code.**

Expressed against real revm state, for a newly entered frame `f` with parent `p`:

```
is_direct_delegatecall_of(A)  ⟺
      f.scheme == CallScheme::DelegateCall
   && p exists
   && p.target_address   == A          // A is the execution context
   && p.bytecode_address == A          // and p was running A's OWN code
   && f actually entered the interpreter            // C12
```

Three consequences follow, and each is a test:

1. **Nesting is excluded for free.** If `p` is itself a delegate frame, then
   `p.bytecode_address != p.target_address`, so an implementation's own nested delegatecall
   can never be attributed to the proxy. (Test 6.)
2. **`CALLCODE` is excluded structurally**, not by an address comparison: it is a distinct
   `CallScheme` variant. (Test 3 / §6.2.)
3. **"Entered execution" is not "the hook fired."** C12 makes the two mechanically
   distinguishable, so a depth-limited, precompile or empty-code delegatecall is never
   observed. (§6.5.)

EIP-7702 is **not** a direct delegatecall: a 7702-delegated `CALL` to A is an ordinary call
whose executed code happens to be the delegate's. It produces **no** `direct_delegatecalls`
observation, because `f.scheme` is `Call`, not `DelegateCall`. (Test 19, added below.)

---

## 5. Approaches considered

### 5.1 Rejected — read the implementation pointer from storage
Resolve A's EIP-1967 slot before or after execution and compare it to an allowlist.
**Rejected on the threat model.** The in-transaction upgrade→execute→rollback sequence
(source design §3.3, test 7) leaves an approved pointer in storage at both observation
points while unapproved code ran in between. It also silently fails for every non-standard
proxy. The source design lists slot reading as an explicit non-goal.

### 5.2 Rejected — capture every frame, filter afterwards
Record all `DELEGATECALL` frames and intersect with the alias set at screening time.
**Rejected on anti-goal A5 and on budget safety.** Per-frame recording cost would be paid by
every deployment including those with no callstack rule, and the observation budget would be
driven by transaction shape rather than by rule content, so an unrelated deep transaction
could exhaust the budget and fail closed. Selector-scoped staging (§6.5) bounds the work by
the rule set.

### 5.3 Rejected — bind `matched` / null-bind an absent observation
Expose a `matched` boolean (or bind the four fields to `null`) and let `condition` test it.
**Rejected on C6 and on the source design's §2.3 field list.** The source design explicitly
does **not** provide `matched`, and C6 shows that leaving the four fields null while still
evaluating the rule makes the canonical deny shape
`{"!": {"in": [{"var": "proxy1.delegateCodeHash"}, [approved…]]}}` evaluate **true** for
every transaction that never touched the proxy — anti-goal A1, a production-wide false deny.
Keeping the fields but suppressing the candidate (§6.4) removes the hazard without adding a
field the source design forbids.

### 5.4 **Chosen** — selector-scoped, execution-confirmed observation with candidate suppression
Stage a candidate only for a frame whose parent context address is in the active snapshot's
alias set; commit it only when `initialize_interp` proves the frame executed; bind the four
real execution values; and when an alias produced no observation, **do not evaluate that
rule at all**. Carry observations to screening through the existing, already-fail-closed
`CaptureOutcome` channel, and give the audit RPC the same capture so §6 and §7 share one
implementation.

**Why this one:** it is the only approach that satisfies A1–A5 simultaneously. A2 is satisfied
because `Observations` is a tri-state, not an `Option`; A3 because the hash comes from
`bytecode_slice()` of the executing frame (C16) and never from `AccountInfo.code_hash` (C17);
A5 because an inert alias set skips all frame work before any per-frame allocation.

---

## 6. Design — ordinary transaction path

### 6.1 Wire schema (normative)

`callstack` is an optional rule section. Under `protocol_version = 1` it admits **exactly one**
field, `direct_delegatecalls`:

```json
{
  "id": "proxy-codehash-guard",
  "event_abis": {},
  "callstack": {
    "direct_delegatecalls": {
      "proxy1": "0x1111111111111111111111111111111111111111"
    }
  },
  "condition": {
    "!": { "in": [ { "var": "proxy1.delegateCodeHash" },
                   [ "0xbbbb…bbbb", "0xcccc…cccc" ] ] }
  },
  "action": "deny"
}
```

Rust wire types, added to `crates/rcs-filter/src/rules/model.rs`:

```rust
/// The `callstack` rule section. Under protocol_version 1 the ONLY admissible field is
/// `direct_delegatecalls`; any other key rejects the rule (V-7).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawCallstack {
    pub direct_delegatecalls: BTreeMap<String, String>,
}

pub struct RawRule {
    // … existing fields unchanged …
    #[serde(default)]
    pub callstack: Option<RawCallstack>,
}
```

`#[serde(deny_unknown_fields)]` on `RawCallstack` is what makes a typo such as `direct_delegatecall`
or `directDelegatecalls` a **rejection** rather than a silent ignore. It is applied to
`RawCallstack` only; `RawRule` keeps its permissive derive so no already-deployed rule shape
regresses (§6.1.1).

#### 6.1.1 Unknown-field rejection is scoped, deliberately
The source design requires that "unknown fields and typos must be rejected, not silently
ignored." C1 shows `RawRule` ignores unknown top-level keys today. Adding
`deny_unknown_fields` to `RawRule` would reject **every currently deployed rule** carrying
any field this build does not model — an availability regression far outside this change's
scope. Therefore:

- **Normative:** unknown-field rejection applies to the **`callstack` object** (`RawCallstack`),
  which is new in this change and has no deployed shapes to protect.
- A misspelled `callstack` sibling at rule top level (e.g. `callStack`) is **not** detectable
  under C1. A rule that declares neither `event_abis` entries nor `callstack` is rejected by
  V-1 below, which is what makes such a typo surface as a loud rejection rather than a rule
  that silently never matches.

### 6.2 Validation (normative)

Added to `compile_rule_with_interner`, evaluated in this exact order so the reported reason is
deterministic:

| # | Rule | Rejection reason (verbatim) |
|---|---|---|
| V-1 | `event_abis` is empty **and** `callstack` is absent | `"rule declares neither event_abis nor callstack"` |
| V-2 | `direct_delegatecalls` is present but empty | `"direct_delegatecalls is empty"` |
| V-3 | more than `MAX_DIRECT_DELEGATECALL_ALIASES` (= 8) aliases | `"direct_delegatecalls declares {n} aliases; at most 8 are allowed"` |
| V-4 | an alias value is not a lowercase, `0x`-prefixed, 20-byte hex string | `"direct_delegatecalls alias '{a}' is not a lowercase 0x 20-byte address"` |
| V-5 | an alias name collides with an `event_abis` key | `"direct_delegatecalls alias '{a}' collides with an event alias"` |
| V-6 | `action == audit` and `event_abis` is empty | `"direct-delegatecall-only rule cannot be an audit rule"` |
| V-7 | any key other than `direct_delegatecalls` inside `callstack` | serde: `"unknown field \`{k}\`, expected \`direct_delegatecalls\`"` |

**C2 is changed by V-1:** the unconditional `"event_abis is empty"` rejection is replaced by
the conditional V-1. This is the one pre-existing validation behaviour this change alters, and
test 20 pins that a rule declaring neither section is still rejected.

**V-4 is strict by design.** C4 shows `Address::from_str` accepts mixed case. The source design
requires lowercase values, so validation checks the **string form** before parsing:
`s.len() == 42 && s.starts_with("0x") && s[2..].chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())`.
A checksummed address is a **rejection**, not a normalisation — the producer contract owns the
canonical form, and silently accepting both would make two textually different rule sets hash
differently while behaving identically.

**V-3 bounds the combined budget.** 8 aliases matches `MAX_EVENTS_PER_RULE` (C3) and bounds the
per-rule candidate product that §6.6 budgets.

Compiled form:

```rust
pub struct CompiledDirectDelegatecall {
    /// Rule-local alias; the `<alias>` prefix of the four condition variables.
    pub alias: String,
    pub address: Address,
}

pub struct CompiledRule {
    // … existing fields unchanged …
    pub direct_delegatecalls: Vec<CompiledDirectDelegatecall>,
}
```

### 6.3 Observation model and the four fields (normative)

New module `crates/rcs-filter/src/callstack.rs`. It holds **plain primitives only**, so the
screening crate gains no EVM dependency (the same cycle-free split the existing capture seam
uses, C9).

```rust
/// One executed direct DELEGATECALL of a selected address. Every field comes from a real
/// execution frame; none is derived from storage or calldata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectDelegatecallObservation {
    /// Execution context address that issued the DELEGATECALL (= parent frame target_address).
    pub context_address: Address,
    /// keccak256 of the runtime bytecode the PARENT frame actually loaded and executed.
    pub context_code_hash: B256,
    /// Direct DELEGATECALL target (= the new frame's bytecode_address).
    pub delegate_address: Address,
    /// keccak256 of the runtime bytecode the NEW frame actually loaded and executed.
    pub delegate_code_hash: B256,
}

/// Identity of the snapshot whose alias set configured a capture window. The FULL identity —
/// a digest alone is never sufficient.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotStamp {
    pub protocol_version: u32,
    pub content_version: u64,
    /// Order-independent digest of the active alias address set.
    pub alias_set_digest: B256,
}

/// Why a trustworthy observation set could not be produced. Fail-closed input, never "empty".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObservationsUnavailable {
    /// Capture was never armed on this execution path.
    NotCaptured,
    /// Frame stack was unbalanced or exceeded its bound.
    IncompleteFrames,
    /// Per-transaction observation budget exceeded.
    BudgetExceeded { attempted: usize, limit: usize },
    /// Observation stamp did not match the evaluating snapshot.
    SnapshotMismatch,
    /// The execution engine could not return a trustworthy observation.
    EngineError,
}

/// Tri-state, deliberately NOT an Option: "available and empty" and "unavailable" are
/// different verdicts (anti-goal A2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Observations {
    Available { stamp: SnapshotStamp, items: Vec<DirectDelegatecallObservation> },
    Unavailable(ObservationsUnavailable),
}
```

Field sources, binding the four values to real revm state (C15, C16):

| Field | Source |
|---|---|
| `context_address` | parent frame's `target_address` |
| `context_code_hash` | `keccak256(parent_interpreter.bytecode.bytecode_slice())` |
| `delegate_address` | new frame's `bytecode_address` |
| `delegate_code_hash` | `keccak256(new_interpreter.bytecode.bytecode_slice())` |

Both hashes are the hash of bytecode an interpreter actually loaded. `AccountInfo.code_hash`
is **never** read for these fields (C17, anti-goal A3).

Condition variables, flat keys per C5:

| Variable | Bound value |
|---|---|
| `<alias>.contextAddress` | lowercase `0x` 20-byte string |
| `<alias>.contextCodeHash` | lowercase `0x` 32-byte string |
| `<alias>.delegateAddress` | lowercase `0x` 20-byte string |
| `<alias>.delegateCodeHash` | lowercase `0x` 32-byte string |

**Not provided, and must not be added:** `matched`, `callType`, `implAddress`, `implCodeHash`.

### 6.4 Matching: alternatives and candidate suppression (normative)

Extends `matching::evaluate_checked` (C7). Per rule, the candidate product gains one dimension
per declared alias, alongside the existing per-event dimensions:

1. For each declared alias `a`, collect every observation in `Observations::Available.items`
   whose `context_address == a.address`, in **execution order**.
2. **If that list is empty for any declared alias, the rule is not evaluated at all** — `continue`
   to the next rule. No binding is built, `condition` never runs, and the rule contributes
   nothing to the outcome.
3. Otherwise the alias contributes its observations as candidates. `visit_bindings` walks the
   full event × alias product lazily, exactly as it does for events today.
4. `build_bindings` inserts the four flat keys per alias for the bound observation.
5. Action merge is unchanged: any candidate satisfying a `deny` rule short-circuits to
   `MatchOutcome::Deny`; `allow` contributes nothing; `audit` is only reachable for a mixed rule
   (V-6) and takes its payload from the **event** side only.

Step 2 is the mechanical difference from events, and it is deliberate. An event with no match
contributes `vec![None]` and the rule is still evaluated (C7); an **alias** with no observation
suppresses the rule. C6 is the reason: null-binding here would turn the canonical negated
allowlist into a universal deny (A1). Test 3 and test 9 pin this.

For a rule declaring several aliases, step 2 means **all** declared aliases must have produced
at least one observation. The source design states this directly: a rule with `proxy1` and
`proxy2` where only `proxy1` delegatecalled **does not match**, and independent per-proxy
monitoring requires separate rules. Test 9.

### 6.5 Capture (normative)

Extends `RcsInspector` in `crates/builder/src/flashblocks/rcs_capture.rs`.

New state:

```rust
/// Active alias addresses plus the stamp of the snapshot they came from. An inert set makes
/// every frame hook a strict no-op before any per-frame work (anti-goal A5).
pub struct DirectDelegatecallConfig {
    pub aliases: HashSet<Address>,
    pub stamp: SnapshotStamp,
    pub max_observations: usize,
}

// on RcsInspector:
callstack: Option<DirectDelegatecallConfig>,
/// One entry per open CALL/CREATE frame, pushed in `call`/`create`, popped in `*_end`.
frames: Vec<FrameRecord>,
observations: Vec<DirectDelegatecallObservation>,
observation_count: usize,          // never rolls back
callstack_overflowed: bool,        // sticky
frames_unbalanced: bool,           // sticky
```

```rust
struct FrameRecord {
    target_address: Address,
    /// `None` for a CREATE frame.
    bytecode_address: Option<Address>,
    scheme: Option<CallScheme>,
    /// Set in `initialize_interp` — `Some` proves this frame entered the interpreter.
    executed_code_hash: Option<B256>,
    /// Staged candidate: this frame is a direct delegatecall of a selected address, pending
    /// execution confirmation.
    staged: Option<StagedCandidate>,
}

struct StagedCandidate {
    context_address: Address,
    context_code_hash: B256,
    delegate_address: Address,
}
```

Hook behaviour:

**`call`** — after the existing `push_checkpoint()`:
1. Return immediately if `!active` or `callstack` is `None` or `aliases.is_empty()` (A5).
2. Push a `FrameRecord` for this frame.
3. Stage a candidate **iff** `inputs.scheme == CallScheme::DelegateCall`, the parent record
   exists, `parent.target_address ∈ aliases`, `parent.bytecode_address == Some(parent.target_address)`,
   and `parent.executed_code_hash` is `Some(h)`. Stage
   `{ context_address: parent.target_address, context_code_hash: h, delegate_address: inputs.bytecode_address }`.
   Staging allocates no observation and consumes no budget.

**`initialize_interp`** (new hook, C12/C13/C14) — the execution-confirmation point:
1. Return immediately if `!active` or the alias set is inert.
2. Compute `h = keccak256(interp.bytecode.bytecode_slice())` and store it in the top
   `FrameRecord.executed_code_hash`. For a frame carrying **no** staged candidate the hash is
   computed only because a *child* delegatecall may need it as `context_code_hash`; when the
   alias set is inert this never runs at all.
3. If the top record has a staged candidate, **commit** it: the frame provably executed, so
   build the observation with `delegate_code_hash = h` and funnel it through
   `observe_direct_delegatecall` (below).

**`call_end` / `create_end`** — pop this frame's `FrameRecord`. A staged-but-never-committed
candidate is **discarded** here, which is exactly the depth-limit / precompile / empty-code /
failed-value-transfer case (C12). Popping an empty stack sets `frames_unbalanced`.

**Revert is not a rollback for observations.** Unlike the real-log queue, which truncates to
the frame checkpoint on revert (C9), committed observations are **never** truncated and
`observation_count` **never** decrements. An executed-then-reverted delegatecall stays
observable, which is the whole point of test 7.

Single funnel, so counting, filtering and overflow can never drift:

```rust
fn observe_direct_delegatecall(&mut self, obs: DirectDelegatecallObservation) {
    let Some(cfg) = self.callstack.as_ref() else { return };
    self.observation_count += 1;
    if self.observation_count > cfg.max_observations {
        self.callstack_overflowed = true;   // sticky; EVM keeps executing normally
        return;
    }
    self.observations.push(obs);
}
```

A frame-stack bound `MAX_OPEN_FRAMES` (1024, above the EVM's own 1024 call depth) guards the
`frames` vector; exceeding it sets `frames_unbalanced`.

**Budget precedence.** `callstack_overflowed` is reported as
`ObservationsUnavailable::BudgetExceeded`, which is **distinct** from the existing
native-transfer `CaptureOutcome::LimitExceeded`. The two budgets are independent:
`max_callstack_observations_per_tx` is new (§6.8) and the native budget is untouched, so an
event-only deployment's behaviour is bit-for-bit unchanged.

`finish_capture` gains a second return channel. To avoid changing the existing
`CaptureOutcome` variants' meaning, `RcsCaptureControl` gains:

```rust
/// Drains the callstack observation result for the window that just finished. Called
/// immediately after `finish_capture`. Returns `Unavailable(NotCaptured)` when the inspector
/// carries no alias configuration.
fn take_direct_delegatecalls(&mut self) -> Observations;
```

Precedence inside `take_direct_delegatecalls`: `frames_unbalanced` → `IncompleteFrames`;
else `callstack_overflowed` → `BudgetExceeded`; else `Available { stamp, items }`. Every exit
path clears the callstack state, matching the existing uniform-cleanup discipline of
`finish_capture`.

#### 6.5.1 One snapshot configures capture and screening (normative)

C10 is a real hazard: the inspector is built once per `execute_best_transactions` while the
snapshot can hot-swap mid-loop. Therefore:

1. An observation set is **stamped** with the full identity of the snapshot that produced its
   alias set — `protocol_version` + `content_version` + `alias_set_digest`. A digest alone is
   never sufficient.
2. For a **fresh** candidate the snapshot is acquired **once, before simulation**, used to
   configure capture, and the **same `Arc<RuleSet>`** is then used for evaluation. The
   post-simulation `current_rules()` acquisition in `screen_tx` must not introduce a second,
   different snapshot for a transaction whose capture already happened.
3. For a **buffered/`Approved`** re-evaluation the capture configuration is derived from the
   `rule_snapshot` saved on the pool entry, and the observations must carry **that** snapshot's
   stamp.
4. A stamp mismatch between the observation set and the evaluating snapshot **fails closed**.
   It must never be resolved by suppressing candidates or by treating the set as empty — that
   would silently convert a configuration error into a clean `allow`.
5. **Every simulation re-captures its own observations.** Observations are execution facts
   about one specific simulation and are never reused across simulations — not across a hot
   reload, and not for an `Approved` re-simulation of the same transaction.

Mechanically, `RcsCaptureControl` gains
`fn configure_direct_delegatecalls(&mut self, config: Option<DirectDelegatecallConfig>)`, called
**per candidate immediately before its simulation** from the candidate loop, so a fresh
candidate and an `Approved` re-simulation in the same round can never share one configuration.
`alias_set_digest` is computed over the sorted, deduplicated alias address bytes, so it is
order-independent and two textually different rule sets with the same alias set share a digest.

### 6.6 Combined budget (normative)

`max_event_bindings_per_tx` already bounds the event candidate product (C7). It is extended to
bound the **full event × alias product**:

```
rule_binding_count = Π(event candidate counts) × Π(alias observation counts)
```

computed with the existing `checked_binding_count` / `checked_add` discipline, so an overflow
in either dimension yields `MatchError::BindingCountOverflow` or
`MatchError::BindingBudgetExceeded` and fails closed through the existing path. A suppressed
rule (§6.4 step 2) contributes **zero**, not one — it is never evaluated.

### 6.7 Threading observations to screening (normative)

C8: `ScreenInput` gains one field.

```rust
pub struct ScreenInput<'a> {
    // … existing fields unchanged …
    /// Direct-delegatecall observations for this simulation. `Unavailable` is fail-closed
    /// input, never "nothing matched".
    pub direct_delegatecalls: &'a Observations,
}
```

`screen_tx` and the `Approved` consistency recompute both pass it to `try_evaluate`. In
`try_evaluate`:

- `Observations::Unavailable(reason)` and the rule set declares **at least one** alias ⇒
  return a new `MatchError::ObservationsUnavailable { reason }`, which the existing error
  handling maps to fail-closed `Screen::Deny`.
- `Observations::Unavailable(_)` and **no** rule declares an alias ⇒ proceed unchanged. An
  event-only deployment keeps today's exact behaviour and pays nothing.
- `Observations::Available { stamp, .. }` whose `stamp` ≠ the evaluating snapshot's stamp ⇒
  `MatchError::ObservationsUnavailable { reason: SnapshotMismatch }`.

`LocalDenyReason` gains `CallstackObservationUnavailable`, and `fail_closed_screen` (C11) gains
the matching arm so each fail-closed decision point emits exactly one bounded `error!` and one
metric. Diagnostic fields are bounded to `tx_hash`, the reason discriminant, and
`attempted`/`limit`; observation contents, addresses and hashes are **never** logged.

### 6.8 Configuration (normative)

```
--rcs-filter.max-callstack-observations-per-tx   (default 10000, env RCS_MAX_CALLSTACK_OBSERVATIONS_PER_TX)
```

`FilterConfig::max_callstack_observations_per_tx: usize`, validated `>= 1` alongside the
existing budgets (C22). The native-transfer budget is untouched.

### 6.9 Inert path (normative)

When no valid rule in the active snapshot declares `direct_delegatecalls`,
`configure_direct_delegatecalls(None)` is installed. Every new frame hook returns before any
frame-state work, `frames`/`observations` stay empty, no hash is computed, and
`take_direct_delegatecalls()` returns `Unavailable(NotCaptured)` — which §6.7 ignores because no
rule declares an alias. This is anti-goal A5 and is asserted by a construction-site
observable, not by a derived counter.

### 6.10 Metrics

New counters on `RcsFilterMetrics`:
`callstack_deny_total`, `callstack_observation_budget_overflow_total`,
`callstack_observations_unavailable_total`, `callstack_snapshot_mismatch_total`.
Fixed, low-cardinality names only; never a dynamic rule id or address label.

---

## 7. L1 deposit path — `xlayer_auditTransactions`

### 7.1 RPC contract (normative)

The method name, request field names and response shape are **unchanged**:

```json
{
  "rules": [ /* rule objects isomorphic to GET /rules, incl. callstack.direct_delegatecalls */ ],
  "txs": [ /* existing deposit shape, unchanged */ ],
  "known_allowed": [ /* unchanged semantics */ ]
}
```

```json
{ "results": [ { "source_hash": "0x…", "tx_hash": "0x…", "verdict": "deny", "actions": {} } ] }
```

- `rules` elements are **completely isomorphic** to `GET /rules` rule objects. They are parsed
  by the same `load_rules`, so §6.1/§6.2 apply verbatim and the request can **never** be
  trimmed to event-only.
- This design adds **no** audit-specific callback, proxy rule format, or extra request field.
- A direct-delegatecall-only rule yields only `allow` or `deny` and **never** an audit action
  payload (V-6 makes such a rule a load error, §7.4).

### 7.2 Request validation (normative)

C19 already rejects the whole request with `-32602` when any rule fails validation; that is
retained and is what makes V-1…V-7 observable on this path. Additionally:

- An unsupported `protocol_version` or an unknown field inside `callstack` produces an
  identifiable `-32602` error naming the offending rule ids and reasons. It is **never**
  ignored-and-continued.
- The handler installs and commits **nothing** when it returns this error.

### 7.3 Ordered simulation with real capture (normative)

C18 is the gap this section closes. The audit handler's EVM is built **with** an `RcsInspector`:

```rust
let inspector = RcsInspector::new_callstack_only(capture_cfg.max_callstack_observations_per_tx);
let mut evm = this.evm_config().evm_with_env_and_inspector(&mut state_db, evm_env, inspector);
```

`RcsInspector::new_callstack_only` is a constructor for a new
`CaptureMode::CallstackOnly`:

- `observe_native_transfer`, `record_real_log` and `push_checkpoint` become genuine no-ops,
  the real-log queue stays empty, `observed_count` / `overflowed` are untouched, and
  `finish_capture` skips both the native-budget check and the real-log cross-check.
- The **frame logic of §6.5 is the same code**, so identification is structurally identical on
  both paths (G6). This is what makes A4 testable rather than asserted.
- `CaptureMode::NativeAndCallstack` is exactly today's behaviour and remains the mode the
  flashblocks candidate loop uses, so the §6 ordinary path is bit-for-bit unchanged.
- Consequence, stated rather than implied: the audit path screens against the raw
  `result.logs()` it already uses (C18/C21). No log channel changes, so an **event-only** rule's
  verdict on this path is unchanged.

Per-deposit loop, extending C20:

1. Compile the request's rules once; derive the request's own `SnapshotStamp` as
   `(protocol_version, content_version = 0, alias_set_digest)` — matching today's
   `load_rules(1, 0, req.rules)` (C19). The request does **not** accept a `content_version`;
   honouring one would imply a persistence this path does not have.
2. If no rule declares an alias, install `None` and the inert path of §6.9 applies: **no
   inspector work at all**, and the response is byte-identical to the base SHA.
3. For each deposit **in request order**: arm capture, `evm.transact(tx)`, finish capture, drain
   `take_direct_delegatecalls()`, classify through `verdict_for`.
4. Each deposit's observations belong to **that deposit only**: the capture window is per
   transaction and every window begins with a full reset (C9).
5. State commit follows C20 unchanged — `Allow` commits, `Deny` discards and continues,
   `Audit` discards and stops. **A prior deposit's committed state is therefore visible to
   every later deposit**, so a deposit that upgrades a proxy makes later deposits resolve the
   new, real delegate target and its real code hash. Test 16.

### 7.4 Verdict mapping (normative)

`verdict_for` passes the drained `Observations` into the same `try_evaluate` (C21), so §6.4 and
§6.7 govern this path verbatim.

| Situation | Verdict |
|---|---|
| direct-delegatecall condition satisfies a `deny` rule | `deny` |
| approved implementation; no rule denies | `allow` |
| `Observations::Unavailable(_)` and some rule declares an alias | `malformed` (non-verdict), state **not** committed |
| snapshot stamp mismatch | `malformed`, state not committed |
| `evm.transact` error | propagated as today; never `allow` |
| direct-delegatecall-only `audit` rule in the request | request-level `-32602` rule error (V-6) — no results |

**`allow` is never returned for an untrustworthy observation.** The existing `Malformed`
verdict is reused as the explicit non-verdict rather than inventing a new wire value, which
keeps the response shape unchanged (§7.1). RCS Force-Tx must **not** treat this error as
`allow`; that is the producer-side obligation recorded in §10.

Budget value semantics on this path: `max_callstack_observations_per_tx` is read from the same
`--rcs-filter.*` argument group and threaded in through a small `AuditCaptureConfig`, because
`XlayerAuditRpc<T>` carries no configuration today. The audit RPC is **not** gated by
`rcs-filter.enabled` — it adjudicates the rules carried in the request, not the node's
installed snapshot, so it must keep working with the filter disabled. Only the budget value is
read from that group.

---

## 8. Complete rule examples

### 8.1 Multi-hash allowlist
```json
{ "id": "proxy1-codehash-allowlist", "event_abis": {},
  "callstack": { "direct_delegatecalls": { "proxy1": "0x1111111111111111111111111111111111111111" } },
  "condition": { "!": { "in": [ { "var": "proxy1.delegateCodeHash" }, [ "0xaaaa…aaaa", "0xbbbb…bbbb" ] ] } },
  "action": "deny" }
```
Delegate hash A or B → rule does not match, transaction continues. Hash C → `deny`. No direct
delegatecall → rule **not evaluated** (§6.4 step 2).

### 8.2 Address-and-hash paired allowlist
Prevents the cross combination of address A with hash B, by requiring each `delegateAddress`
to be paired with its own `delegateCodeHash` set inside an `or` of `and`s, negated as a whole.
(Verbatim shape as in the source design §8.2.)

### 8.3 Also validating the proxy's own runtime code
```json
{ "callstack": { "direct_delegatecalls": { "proxy1": "0x1111111111111111111111111111111111111111" } },
  "condition": { "and": [
    { "==": [ { "var": "proxy1.contextCodeHash" }, "0xproxy-code-hash" ] },
    { "!": { "in": [ { "var": "proxy1.delegateCodeHash" }, [ "0xapproved-implementation-hash" ] ] } } ] },
  "action": "deny" }
```

---

## 9. Acceptance tests (normative)

Tests 1–18 are the source design's test matrix, one-to-one. Tests 19–26 are additions this
code-level design requires; each is falsifiable against the pre-change tree.

| # | Scenario | Expected |
|---|---|---|
| 1 | Proxy direct-delegatecalls an **approved** implementation | `allow` |
| 2 | Proxy direct-delegatecalls an **unapproved** implementation | `deny` |
| 3 | Ordinary `CALL` to the proxy, no delegatecall | no observation; rule not evaluated; `allow` |
| 4 | Two allowed delegate hashes | both `allow` |
| 5 | One unallowed delegate hash | `deny` |
| 6 | Implementation performs its own nested delegatecall | no observation **for that alias**; §4 consequence 1 |
| 7 | Same tx upgrades to an unapproved impl, calls it, then **reverts** | `deny` (observation survives the revert) |
| 8 | Same tx makes several delegatecalls, one to an unapproved impl | `deny` (any candidate denying is enough) |
| 9 | One rule declares `proxy1`+`proxy2`; only `proxy1` delegatecalls | rule **not** evaluated → no match; independent monitoring needs separate rules |
| 10 | An **unselected** third proxy delegatecalls | not recorded |
| 11 | Observation budget exceeded | `deny` (`BudgetExceeded`) |
| 12 | Frame stack inconsistent / capture unavailable | `deny` (`IncompleteFrames` / `NotCaptured`) |
| 13 | Snapshot changed and an old observation is presented | refuse to adjudicate → `deny` (`SnapshotMismatch`) |
| 14 | L1 deposit calls the proxy, implementation **approved** | audit RPC `allow` |
| 15 | L1 deposit calls the proxy, implementation **unapproved** | audit RPC `deny` |
| 16 | An earlier deposit upgrades the implementation; a later deposit calls the new one | later deposit judged against the **new** implementation |
| 17 | Audit RPC receives an unsupported protocol version or an unknown `callstack` field | `-32602` rule error; never `allow` |
| 18 | direct-delegatecall-only `audit` rule | rule load failure (V-6) |
| 19 | EIP-7702 delegated `CALL` to a selected address | **no** `direct_delegatecalls` observation (`f.scheme == Call`) — §4 |
| 20 | Rule declaring neither `event_abis` entries nor `callstack` | rejected with V-1's reason (pins the C2 change) |
| 21 | A checksummed / uppercase alias address | rejected with V-4's reason |
| 22 | A 9th alias; and an alias colliding with an event alias | rejected with V-3 / V-5 reasons |
| 23 | `DELEGATECALL` that never entered the interpreter (depth limit, precompile, empty code) | no observation (C12 staging/discard) |
| 24 | `CALLCODE` from a selected address | no observation (distinct `CallScheme`) |
| 25 | Inert path: snapshot with no alias; event-only rule with a non-zero internal native transfer | verdict and produced block byte-identical to the base SHA; no inspector frame work (construction-site observable) |
| 26 | Same rule, same execution frames, both paths | the §6 ordinary-path and §7 audit-path verdicts are **equal** (A4 / G6) |

Tests 11–13 and 23–25 are the ones that most easily pass for the wrong reason; each must be
written to fail against the pre-change tree with a recorded failure count.

---

## 10. Interfaces this design does not own

| Id | Obligation | Owner |
|---|---|---|
| CT-A | Alias values published lowercase, `0x`-prefixed, 20 bytes; ≤ 8 aliases per rule; no alias/event-alias collision | `xlayer-rcs` Rule Producer |
| CT-B | `callstack` carries no key other than `direct_delegatecalls` | Rule Producer |
| CT-C | Never publish a direct-delegatecall-only `audit` rule | Rule Producer |
| CT-D | RCS Force-Tx must **not** treat the audit RPC's non-verdict as `allow` | RCS Force-Tx |
| CT-E | Consumer deployed before the producer enables such rules (no capability negotiation) | Release owner, §11 |

Joint acceptance with `medivac-directcallproducer-starport-rcs` is **INTEGRATION_PENDING**:
both sides must pin exact versions before it can be claimed.

---

## 11. Release order (normative)

1. Ship an `xlayer-reth` version supporting **both** the ordinary Filter and the audit RPC.
2. Verify both paths parse `callstack.direct_delegatecalls` under the existing protocol.
3. Only then deploy the `xlayer-rcs` producer.
4. Publish a rule containing only approved implementations; verify `allow`.
5. Switch to an unapproved implementation; verify **both** ordinary transactions and L1
   deposits `deny`.
6. Verify no false positives for no-proxy, ordinary `CALL`, and nested delegatecall.
7. Enable the production rule.

---

## 12. **Open decision — convergence with the approved `callstack` design on the same branch**

This is the one item this design cannot settle by itself, and it must be decided before
implementation.

**The facts.** MR !79 — *"RCS Filter 通用 callstack 规则与 Proxy codeHash 风控能力 XLOP-1239"* —
is **open**, not merged: `source_branch` `xl/xlop-1239/1027-1118`, `head_sha`
`3a6028eb13c14c4fc1dbc365fc964636dadf6bfe`, `base_sha` `d32b20beed8c8b56a2449f00a24fe26288a55d48`
(identical to this design's inspected base), `merged_at: null`, same Epic XLOP-1239. Its
approved design is https://okg-block.sg.larksuite.com/docx/NVVmdAwzbo7gMcxjzk6l9N49g3f r3
(canonical sha256 `614ab3b4…bcbc`), which `starport-rcs` PASSed on 2026-10-27. A second
approved design, https://okg-block.sg.larksuite.com/docx/Ck6pdzxrAoprk8xm7rHlkPPbgUc r5
(`75a757b7…1874`, PASSed 2026-10-28, XLOP-1241), builds further on `3a6028eb` and adds real
capture to the audit RPC under `protocol_version: 2` plus `known_allowed_kinds` (CT-11).

Because `3a6028eb` is unmerged, `main` at the inspected base SHA contains **no** `callstack`
section at all — consistent with C1/C2 above. So nothing in §6/§7 conflicts with the code as it
stands today. The conflict is about **convergence**, and it is substantive:

| Item | This design (source doc, 2026-10-28) | MR !79 approved r3 (implemented) |
|---|---|---|
| Protocol version | stays **1**; `callstack` is a v1 schema extension | `callstack` **requires 2**; `SUPPORTED_PROTOCOL_VERSIONS = [1, 2]` |
| `callstack` shape | `direct_delegatecalls: map<alias, address>`, the **only** admissible field | named selectors with address + optional `implAddress`/`implCodeHash` |
| Condition variables | `contextAddress`, `contextCodeHash`, `delegateAddress`, `delegateCodeHash` | `matched`, `callType`, `address`, `codeHash`, `implAddress`, `implCodeHash` |
| `matched` | explicitly **not provided** | **required**, on the C6 correctness argument |
| No observation | alias unbound ⇒ rule **not evaluated** (§6.4 step 2) | binds `matched = false` + five nulls; condition **still** evaluated |
| Audit RPC capture | **required** real ordered capture; never `allow` | explicitly **not wired**; non-verdict for selector-bearing rules (XLOP-1241 r5 adds it under v2) |
| Unknown `callstack` key | rule invalid | rule invalid — **consistent** |
| Direct-delegatecall-only `audit` | invalid | callstack-only `audit` rejected — **consistent** |

Rows 1–6 cannot both hold on `main`. Two incompatible `callstack` schemas on one branch would
mean one of them is dead code, and a rule set valid for one would be rejected by the other.

**Options.**

- **O1 — Coexist, additively.** Keep !79's v2 selector form, and add `direct_delegatecalls` as a
  second admissible `callstack` form accepted under v1. Preserves !79's 724 passing tests and
  XLOP-1241 r5's dependency, and keeps this design's v1 requirement. Cost: two observation
  models and two "no observation" semantics in one matcher, and this design's "only
  `direct_delegatecalls`" constraint must be read as scoped to a `direct_delegatecalls` rule
  rather than to the `callstack` object globally.
- **O2 — This design supersedes.** Narrow `callstack` to `direct_delegatecalls` only, under v1,
  and drop `matched` / `callType` / `impl*`. Exactly faithful to the newer, Player-authorized
  source doc. Cost: invalidates an approved design, a tested open MR, and the XLOP-1241 r5
  design built on top of it.
- **O3 — Defer.** Implement §6/§7 on `main` as written, declare convergence INTEGRATION_PENDING,
  and reconcile when !79 merges. Cost: the reconciliation is then unavoidable and larger, and
  whichever side merges second rewrites the other's wire contract.

**Recommendation: O1.** It is the only option that breaks nothing already approved and already
tested, and it still delivers every requirement of the source design — including the genuinely
new capability, which is §7.3's real ordered capture for L1 deposits (`main` and !79 both lack
it). Under O1, §6.1's "`callstack` admits exactly one field" is implemented as: *a `callstack`
object is either a `direct_delegatecalls` object or a v2 selector object, never a mixture, and
each form rejects keys foreign to it.* Everything else in §6 and §7 stands unchanged.

The reviewing building approved both sibling designs and is the right authority to choose.
**A PASS on this revision is read as approving O1** together with §§1–11; a different
convergence target requires `CHANGES_REQUESTED` naming it.

---

## 13. Evidence, limits and what is left to the implementer

### 13.1 Evidence boundary
- Source design read in full from Lark (`ODE0deAHnoyEfcxixyIlkyl4gWf`, modified
  `2026-10-28T06:44:09Z`), all 11 sections and the 18-row test matrix.
- Repository grounded through a CodeGraph index of this checkout (7,370 files / 149,359 nodes /
  475,461 edges) plus direct reads of: `rules/{model,validation,mod,jsonlogic}.rs`,
  `matching.rs`, `handle.rs`, `config.rs`, `metrics.rs`, `lib.rs`,
  `builder/src/flashblocks/{rcs_capture.rs (non-test regions), context.rs (capture + loop
  regions)}`, `rpc/src/audit/{rpc.rs, verdict.rs}`.
- Dependency facts C12–C16 read from the locked crates `revm-inspector 21.0.3` and
  `revm-interpreter 37.0.3`, and from the vendored `alloy-op-evm` / `op-revm` under `deps/`.
- MR !79 state read from GitLab (open, `3a6028eb`, base `d32b20be`). Sibling designs r3/r5 read
  from Lark. Jira XLOP-1239 read back as an Epic in project XLOP with the `biz-feature` label.
- **Not done:** no product build, no `cargo test`, no clippy, no e2e, no Sonar, no running node.
  No production or test code was modified in this stage. Canonical hashes quoted for the
  sibling designs are their own publication declarations, not independently recomputed.

### 13.2 Left to the implementer (explicitly not decided here)
- Exact module/file split inside `crates/rcs-filter/src/callstack.rs` and the private helper
  names in `rcs_capture.rs`.
- Whether `alias_set_digest` is keccak over sorted address bytes or another stable 32-byte
  digest, provided it is order-independent and documented.
- Test harness mechanics: which seams are `#[cfg(test)]` thread-locals versus constructor
  parameters. Note the existing constraint that a crate compiled as a dependency does **not**
  enable its own `cfg(test)` and `pub(crate)` does not cross a crate boundary, so a test seam
  needed by `xlayer-builder` tests cannot simply be moved into `rcs-filter`.
- `MAX_OPEN_FRAMES`'s exact value, provided it is ≥ the EVM call-depth limit and the
  exceeded branch fails closed.

### 13.3 Non-authorisation
This is a code-level design for review. It authorises no merge, no deployment, no release and
no scope expansion beyond `xlayer-reth` on `main` under Epic XLOP-1239.
