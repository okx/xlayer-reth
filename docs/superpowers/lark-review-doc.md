# Lark design review document — RCS Filter Direct Delegatecall consumer (XLOP-1239)

This file is the durable record of the ONE Lark Doc that carries the current Spec. Every
revision UPDATES THE SAME DOC and appends a row below; a new Doc is never created for a
revision.

## Current design document

| Field | Value |
|---|---|
| Full verified URL | https://okg-block.sg.larksuite.com/docx/Eo2ldAfN7od95ExYKAWlh2uXgAd |
| Doc token | `Eo2ldAfN7od95ExYKAWlh2uXgAd` |
| Doc type | `docx` |
| Title | `2026-10-28-rcs-filter-direct-delegatecall-consumer-design` |
| Design revision | **r4** (current, **APPROVED**) |
| Canonical content sha256 | `769690d4c31098c3c4dd318cdbbc8ac7d94fa111ca4675becca1f76952be4179` |
| Hash definition | sha256 of the **canonical slice** of the committed spec file: its content from the first `# ` heading onward. The file's leading PUBLICATION BLOCK and the Lark header block are both excluded by construction, so the value is non-self-referential. Recompute with `sed -n '/^# /,$p' <spec file> \| sha256sum` |
| Design path (workspace-relative) | `xlayer-reth/docs/superpowers/specs/2026-10-28-rcs-filter-direct-delegatecall-consumer-design.md` |
| Revision time (Lark) | r4 published 2026-10-28 (document_revision_id 13); r3 at revision 12; r2 at revision 10; r1 imported `2026-10-28T07:26:29Z` |
| Created | `2026-10-28T07:26:29Z` |
| Repository / branch | https://gitlab.okg.com/github/xlayer-reth / `main` |
| Inspected base SHA | `d32b20beed8c8b56a2449f00a24fe26288a55d48` |
| Jira Epic | https://okcoin.atlassian.net/browse/XLOP-1239 |
| Source design (input) | https://okg-block.sg.larksuite.com/docx/ODE0deAHnoyEfcxixyIlkyl4gWf (modified `2026-10-28T06:44:09Z`) |
| Unit / reviewing building | `medivac-directcallconsumer-starport-rcs` / `starport-rcs` |
| Correlation id | `direct-delegatecall-units-20261028-001` |

### Access and completeness verification (performed, not assumed)

**r1 (initial publication, md→docx import).** `lark_doc_get_metadata` returned
`not_found: false`. `lark_docx_raw_content` read the body back in full (50.7 KB) and it was
compared programmatically against the committed Spec file: the complete committed body was
contained in the published document, content-equivalent after normalising only markdown
decoration that the import drops (table separator rows, list bullets, heading markers,
code-fence language tags). All 14 sections and all 26 test rows were present.

**r2 (same Doc, updated in place).** r2 was prepended to **this same document** as the current
revision, ahead of r1 which is retained below an explicit `END OF REVISION r2` separator — the
same convention the sibling RCS designs use. Verified after the update:
- `lark_docx_raw_content` read the whole document back (93,955 characters at r2; **114,661
  characters at r3**).
- The temporary probe block used to validate the block shape was deleted and is confirmed absent.
- r2 precedes the retained r1, and an explicit reading rule in the r2 header states that the
  block further down still titled "REVISION r1 — CURRENT REVISION" is superseded history.
- **r3 structure verified by its real separator lines:** exactly six separators in the order
  `REVISION r3` → `END OF REVISION r3` → `REVISION r2` → `END OF REVISION r2` → `REVISION r1` →
  `END OF REVISION r1`, so the current revision is unambiguously first and both prior revisions
  are retained history. All 43 r3 identifiers (R1–R4, the new `ResultAlreadyTaken` /
  `OwnCode` / `AuditRequestEnvelope` / `clear_active_window` names, tests 37–39 and 48–59, the
  r3 canonical hash and the base SHA) are present in the r3 region.
- Programmatic coverage check at r2: **all 84 normative identifiers** extracted from the committed r2
  revision (V-0…V-11, D0…D5, CT-A…CT-G, A1…A5, every `§` reference, every test number, and the
  new type/field names such as `OwnCodeIndeterminate`, `ResultAlreadyTaken`,
  `RULE_LEGACY_TOLERATED_KEYS`, `alias_map_reject_duplicates`, `CaptureFaults`,
  `AuditCaptureConfig`, `xlayer-rcs-capture`) appear in the published document, and **every
  evidence reference** (the four locked crate versions, each cited source file, the MR !79 head
  SHA and the inspected base SHA) appears. Acceptance-test rows 1–47 are all present.
- **Fidelity note, stated rather than implied:** r2 was written into the Doc as plain-text
  blocks, so it is *content-equivalent* to the committed markdown rather than byte-identical in
  decoration (markdown tables render as aligned text). The committed file remains the canonical
  artifact, and the canonical sha256 above identifies that file — not the Lark rendering. This
  matches the sibling convention that the canonical value is the consistent document/request
  declaration.
- This is a `docx` design document URL. It is **not** a Jira URL, an Oli Task URL, an MR URL,
  a hostname-only URL, or a truncated preview.

## Revision history

| Revision | Doc token | Canonical sha256 | Published | Decision | Decision evidence |
|---|---|---|---|---|---|
| r1 | `Eo2ldAfN7od95ExYKAWlh2uXgAd` | `aaeede4912ce72487e1e612223e6d821815c3cca9ab45eee181c216fdac8e9d1` | 2026-10-28T07:26:29Z | **CHANGES_REQUESTED** (convergence option **O2** selected; five technical groups) | Bus reply `starport-rcs-directcallconsumer-spec-r1-changes-20261028` / record `reczz28Skbf6IAfL`, `reply_to` = `medivac-directcallconsumer-starport-rcs-stage1.0-specreview-r1`, sender exactly `starport-rcs`. Full body read back and recorded in `terran/agents/starport-rcs/memory/units/medivac-directcallconsumer-starport-rcs/reviews/stage1.0-spec-r1-changes-20261028.md` |
| r2 | `Eo2ldAfN7od95ExYKAWlh2uXgAd` | `dc94bbc3cd10590cb91b156af19050775c56a116724a88c6d8976e62e0d0d829` | 2026-10-28, same Doc, doc revision 10 | **CHANGES_REQUESTED** (two lifecycle/validation-order contradictions; two interface details settled by the reviewer) | Bus reply `starport-rcs-directcallconsumer-spec-r2-changes-20261028` / record `reczz28Sl2W46ZVp`, `reply_to` = `medivac-directcallconsumer-starport-rcs-stage1.0-specreview-r2`, sender exactly `starport-rcs` |
| r3 | `Eo2ldAfN7od95ExYKAWlh2uXgAd` | `6049162411d1ae8838451738cf0b76a74b650f9278906f056345cd93dad5019d` | 2026-10-28, same Doc, doc revision 12 | **CHANGES_REQUESTED** (one blocker: Phase A's `Vec<serde_json::Value>` destroyed duplicate-alias evidence) | Bus reply `starport-rcs-directcallconsumer-spec-r3-changes-20261028` / record `reczz28SlQjtgpho` |
| **r4 (current)** | `Eo2ldAfN7od95ExYKAWlh2uXgAd` | `769690d4c31098c3c4dd318cdbbc8ac7d94fa111ca4675becca1f76952be4179` | 2026-10-28, same Doc, doc revision 13 | **PASS — APPROVED** | Bus reply `starport-rcs-directcallconsumer-spec-r4-pass-20261028` / record `reczz28SlhkREAjb`; see `## Decision — r4` |

## Decision — r1

- Reviewer (routed recipient / actual sender): `starport-rcs`
- approval_mode: `delegated_agent`
- Request `message_id`: `medivac-directcallconsumer-starport-rcs-stage1.0-specreview-r1`
  (record `reczz28SkU8ZkAsf`)
- Reply `message_id`: `starport-rcs-directcallconsumer-spec-r1-changes-20261028`
  (record `reczz28Skbf6IAfL`), `reply_to` matches the request exactly
- `correlation_id`: `direct-delegatecall-units-20261028-001`
- **Decision: CHANGES_REQUESTED — NOT a PASS.** Convergence option **O2** was selected (the v1
  `direct_delegatecalls` design supersedes the v2 selector form); O1 and O3 were explicitly not
  approved. Five technical groups were raised and all five were independently verified against
  the real code before being accepted into r2.
- This decision binds to revision r1 /
  `aaeede4912ce72487e1e612223e6d821815c3cca9ab45eee181c216fdac8e9d1` only, and does **not**
  carry forward to r2.

## Decision — r2

- Reviewer (routed recipient / actual sender): `starport-rcs`; approval_mode `delegated_agent`
- Request `message_id`: `medivac-directcallconsumer-starport-rcs-stage1.0-specreview-r2`
  (record `reczz28SkvOhg6XM`)
- Reply `message_id`: `starport-rcs-directcallconsumer-spec-r2-changes-20261028`
  (record `reczz28Sl2W46ZVp`), `reply_to` matches the request exactly
- **Decision: CHANGES_REQUESTED — NOT a PASS.** Most r1 items were closed at the design level.
  Two contradictions remained (the `finish`/`take` parking-vs-cleanup conflict, and the claimed
  validation priority being incompatible with eager serde decoding), and two interface details
  were settled by the reviewer (CT-F allowlist is empty; V-9 narrowed to non-empty). Both
  contradictions were independently verified against the real code before being accepted.
- The `known_allowed` qualified contract was explicitly **accepted**, with its limitation made a
  standing disclosure that must appear in the interface, test 46 and the final report.
- Binds to revision r2 only; does **not** carry forward to r3.

## Decision — r3

- **CHANGES_REQUESTED.** One blocker: r3's Phase A used `Vec<serde_json::Value>`, which collapses
  duplicate keys (serde_json 1.0.150 last-wins), destroying V-10's evidence. Independently
  verified against the checksummed dependency source, then fixed in r4.
- Reply `starport-rcs-directcallconsumer-spec-r3-changes-20261028` / record `reczz28SlQjtgpho`.
- Binds to r3 only.

## Decision — r4 — **APPROVED**

| Field | Value |
|---|---|
| Decision | **PASS** — verbatim: `PASS：设计没问题，继续。` |
| approval_mode | **`delegated_agent`** (the task creator delegated this design-stage review to the parent building through this Flow) |
| Reviewer (routed recipient and actual reply sender) | **`starport-rcs`** — the assigned `BUILDING_ID`. No human signed this; it is an Agent decision and is recorded as such. |
| Approved revision | **r4** |
| Approved canonical sha256 | `769690d4c31098c3c4dd318cdbbc8ac7d94fa111ca4675becca1f76952be4179` |
| Approved document URL (full, as cited in the reply) | https://okg-block.sg.larksuite.com/docx/Eo2ldAfN7od95ExYKAWlh2uXgAd |
| Request `message_id` | `medivac-directcallconsumer-starport-rcs-stage1.0-specreview-r4` (record `reczz28SlbtJiyfZ`) |
| Reply `message_id` | `starport-rcs-directcallconsumer-spec-r4-pass-20261028` (record `reczz28SlhkREAjb`) |
| `correlation_id` | `direct-delegatecall-units-20261028-001` |
| `reply_to` | `medivac-directcallconsumer-starport-rcs-stage1.0-specreview-r4` — matches the request exactly |
| Base SHA confirmed by the reviewer | `d32b20beed8c8b56a2449f00a24fe26288a55d48` |

**Verification performed, not assumed.** The hash cited in the PASS was recomputed from the
committed spec file (`sed -n '/^# /,$p' <spec> | sha256sum`) and matches byte for byte, so the
approval is bound to the *current* revision and is not a stale or different-revision PASS. The
reply's envelope was checked field by field: sender is exactly the assigned building, recipient is
this unit, the correlation id matches, and `reply_to` is the exact r4 request id. The full
document URL appears in the reply (not a truncated preview).

**What the PASS authorises:** implementation by this unit, on a **new branch** cut from the
approved `xlayer-reth`/`main` scope, against r4 and its inherited acceptance, under Epic
XLOP-1239. Afterwards the exact MR SHA, real compile/test logs and memory-publication evidence go
back to the building for a separate review.

**What it does NOT authorise:** it is explicitly **not** a final MR approval, **not** merge, and
**not** deployment. No scope expansion.

**Two implementation notes** arrived in the approving message and were explicitly marked as
applying directly under the existing spec with **no further design round required**. They are
recorded in `xlayer-reth/docs/superpowers/post-pass-implementation-notes.md` rather than edited
into the Spec, because editing the Spec would change the approved canonical hash and break the
binding.

**Three standing boundaries remain in force** and must be carried into test 46 and the final
report: (1) a deposit already in `known_allowed` executing an unapproved implementation is still
not denied; (2) RCS degradation / fail-open does not mean end-to-end protection; (3) joint
acceptance with the producer is INTEGRATION_PENDING.

## Rules for future revisions (binding)

1. Update **this same Doc** (`Eo2ldAfN7od95ExYKAWlh2uXgAd`); never publish a second Doc.
2. Bump the revision label, recompute the canonical sha256 of the committed spec file, and
   append a new row to the revision history above.
3. Re-verify URL/access and full-content readback before requesting review again.
4. A PASS binds to exactly one revision + canonical hash. A PASS for an earlier revision
   never carries forward.
