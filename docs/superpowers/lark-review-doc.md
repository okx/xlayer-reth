# Lark Design Review Document

- **Full URL:** https://okg-block.sg.larksuite.com/docx/UXohdEdeaokIGpxWnJPlaqGugac
- **Doc token:** `UXohdEdeaokIGpxWnJPlaqGugac`
- **Title:** MR77-RCS-native-transfer-fixes-design-r1-20261012
- **Current revision:** r11 (2026-10-13) — unchanged in Stage 1.0 attempts 10 and 11 (no r12; design not reopened)
- **Canonical spec content hash (sha256, r11):** `49dce95ee1dd5f4f37fcabd0ae24c084447a10b111cc7acaea20f8b141970836`
- **In-repo design path:** `docs/superpowers/specs/2026-10-12-rcs-native-transfer-mr77-fixes-design.md`
- **Byte-identical approved body:** `docs/superpowers/specs/2026-10-12-rcs-native-transfer-mr77-fixes-design-r11-canonical-body.txt` (sha256 matches the canonical hash above)
- **Lark last_modified at PASS:** 2026-10-13T10:15:56Z (read back live in attempt 9; independently read back by the reviewer)
- **Plan / implementation baseline:** `ae490d7ffebce921a8e957618b56672d192739f9` (current MR77 head, verified live 2026-10-14) — supersedes the earlier `a0e5d7b7` and `21a311e9` notes. Build on top of it; no force-push, no push to `main`, no second MR.
- **Approval mode:** `delegated_agent` — PASS received (`starport-rcs-design-review-r11-pass-20261013`, Bus `recvxVksp5gum4`)
- **Reviewer:** building `starport-rcs` (via Terran Bus). No human signature; none required for this design stage.
- **MR:** https://gitlab.okg.com/github/xlayer-reth/-/merge_requests/77
- **Jira:** XLOP-1228 under Epic XLOP-1217

## Revision History

| Revision | Date | sha256 | Decision | Bus record_id |
|---|---|---|---|---|
| r1 | 2026-10-12 | 6ff06500... | CHANGES_REQUESTED | recvxOPezyfmOF |
| r2 | 2026-10-12 | c5a571e9... | CHANGES_REQUESTED | recvxP4MqPZ68s |
| r3 | 2026-10-12 | b64edba4... | PASS | recvxP9lTBdjfE |
| r4 | 2026-10-12 | 7a788723... | PASS | recvxPWf3nDvmr |
| r5 | 2026-10-13 | e21ac26a... | CHANGES_REQUESTED | recvxThvIciWWP |
| r6 | 2026-10-13 | b4303bf8... | CHANGES_REQUESTED | recvxTnGhvAaCQ |
| r7 | 2026-10-13 | ed1e3daf... | PASS | recvxTrr2W5gUl |
| r8 | 2026-10-13 | cce8b3ac... | CHANGES_REQUESTED | recvxUBDyttm2E |
| r9 | 2026-10-13 | 7321a778... | CHANGES_REQUESTED | recvxUH7IxcuCV |
| r10 | 2026-10-13 | 87cb906d... | PASS | recvxUNFLdFDzO |
| **r11 (current)** | **2026-10-13** | **49dce95e...** | **PASS** | **recvxVksp5gum4** |

## Verification record — Stage 1.0 attempt 10 (2026-10-14)

No design change was made in this attempt: no new rework feedback was delivered to Stage 1.0, the design revision
stays r11, and therefore no new Lark publication and no new Bus review request were required.

### Verified in this attempt (method named per item)

| Item | Result | Method |
|---|---|---|
| Explicit PASS text for r11 | `PASS：设计没问题，继续` — present verbatim | Full read of the reviewer's own committed record `agents/starport-rcs/memory/sessions/mr77-design-review-r11-20261013.md` on `jimmy.shi/terran@master` via GitLab MCP (blob `510bc58d...`, last_commit `3823d10d`) |
| Reviewer identity = BUILDING_ID `starport-rcs` | Confirmed | Record authored under `agents/starport-rcs/`; `sender=starport-rcs` in the unit ledger |
| Recipient = UNIT_ID | Confirmed `viking-nativetransfer-starport-rcs` | Unit ledger `memory/sessions/design-review-r11-20261013.md` on remote `master` (blob `746f34db...`, last_commit `b4fd6e69`) |
| `correlation_id` | `player-fix-mr77-native-transfer-2items-20261012` | Both ledger records |
| `reply_to` ↔ request message_id | `viking-nativetransfer-starport-rcs-design-review-r11-20261013` — matches on both sides | Both ledger records |
| Reply message_id / Bus record_id | `starport-rcs-design-review-r11-pass-20261013` / `recvxVksp5gum4` | Both ledger records |
| Full design URL in the decision | Full `https://okg-block.sg.larksuite.com/docx/UXohdEdeaokIGpxWnJPlaqGugac` — not truncated, not a Jira/Oli/MR substitute | Reviewer record, verbatim reply block |
| Current revision + content hash | r11, `49dce95e...` | Reviewer record; **and** independently recomputed: the in-repo canonical body hashes to exactly `49dce95ee1dd5f4f37fcabd0ae24c084447a10b111cc7acaea20f8b141970836` |
| MR77 target of the design | opened, `xl/xlop-1217/0928-1135` → `main`, head `a0e5d7b73a669d50ed18f65d1c40c52a7a70a230` | GitLab MCP `get_merge_request` |

### NOT verifiable in this attempt (named limitation)

The declared MCP server `ok-okengine-lark-mcp` exposed **no tools** in this attempt's tool table (attempt 9 had
`lark_docx_raw_content`, `lark_doc_edit`, `lark_bitable_search_records`, `lark_bitable_create_record`,
`lark_bitable_update_record`, `lark_doc_get_metadata` available). Consequently, in attempt 10:

- the Lark Doc was **not** re-read live (URL/access/last_modified not re-confirmed this attempt);
- the Bus row `recvxVksp5gum4` was **not** re-read live;
- no new Lark publication and no new Bus write were possible.

No HTTP/REST fallback was attempted, per the runtime's MCP tool-access rule. The r11 decision itself was
live-authenticated against the Bus in attempt 9; this attempt re-verified it from the reviewer's independent
committed record and re-verified the design content by hash.

## Verification record — Stage 1.0 attempt 11 (2026-10-14)

**This round is NOT a design reopen.** The building's new feedback (dispatched after fresh Player direction)
does not challenge r11. Design revision stays **r11**, canonical sha256 stays
`49dce95ee1dd5f4f37fcabd0ae24c084447a10b111cc7acaea20f8b141970836`, and the **PASS**
(`starport-rcs-design-review-r11-pass-20261013`, Bus `recvxVksp5gum4`) is carried forward unchanged.
No r12 was published, no new Lark write was made, and no new design-review request was sent.

### Verified in this attempt (method named per item)

| Item | Result | Method |
|---|---|---|
| r11 still the building's recorded current decision | `Unit当前决定：starport-rcs-design-review-r11-pass-20261013（recvxVksp5gum4），已readback` | Fresh `git pull` of `jimmy.shi/terran@master` (now at `42ff554`), full read of `agents/starport-rcs/MEMORY.md` |
| The new feedback does not reopen the design | Building's own status report explicitly scopes the round to delivery/evidence gaps, not design | Full read of `agents/starport-rcs/memory/sessions/mr77-current-issues-20261014.md` and `.../mr77-player-focus-20261014.md` |
| Design content integrity | In-repo canonical body re-hashes to exactly `49dce95e…0836` | `sha256sum` on `specs/…-design-r11-canonical-body.txt` in this workspace |
| Current MR77 head | `ae490d7ffebce921a8e957618b56672d192739f9`, state `opened`, `xl/xlop-1217/0928-1135` → `main`, `detailed_merge_status: not_approved` | GitLab MCP `get_merge_request` (live, this attempt) |
| Item 1 (MR body staleness) independently re-confirmed | MR description body still states design revision **r4**, head `21a311e9` and `AC6 — 404/404 total tests pass`, and still lists the per-test name tables | Same live `get_merge_request` response — read in full |

### Source of the six downstream items

`agents/starport-rcs/memory/sessions/mr77-player-focus-20261014.md` (building dispatch
`starport-rcs-viking-focus-reczz28NDDzzYzst-r1`, Bus record `reczz28NDLogM6qj`, correlation `reczz28NCcA0SK1N`)
and `.../mr77-current-issues-20261014.md` (building status report to Player,
`starport-rcs-mr77-current-issues-reczz28NCcA0SK1N-r1`, record `reczz28NDBIBLhVy`), both on
`jimmy.shi/terran@master`, read in full this attempt.

### Scoped downstream requirements (inside the existing r11 acceptance — not new design)

These close `r11-EVIDENCE-CONTRACT`, `r11-NT2` and `r11-MR-TABLE`. Each has one owner stage and one
mechanically checkable pass criterion, so Stage 5.0 can verify them item by item.

| # | Requirement | Owner | Pass criterion |
|---|---|---|---|
| 1 | Rewrite the **MR77 description body itself** (not another note) in revision-independent, stable wording: no hard-coded `rN` design label, no hard-coded test count, no 14 nonexistent test names; reference suites by crate + pass criterion; list wording items and evidence items **separately**; stop reporting "not yet independently re-reviewed" as a code defect. Make notes `11222724` and `11223169` consistent (e.g. post a note that supersedes both). | **4.0** | Live `get_merge_request` shows a description containing no `rN` design-revision label, no fixed total-test count, and none of the removed test names; `get_merge_request_notes` shows a superseding note reconciling 11222724/11223169. Closes `r11-MR-TABLE`. |
| 2 | Resolve the **evidence-doc SHA binding**: the r11 evidence header records capture SHA `ae04e4b3` while the MR note claims `ae490d7f`. Either prove `ae04e4b3..ae490d7f` is docs-only, or re-capture at the exact final head and update the header. | **3.0** | Evidence doc header SHA == final head, **or** the doc carries a verifiable `git diff --name-only ae04e4b3..ae490d7f` output showing only `docs/**` paths. |
| 3 | Attach the **real output** of `cargo tree -p xlayer-reth-node -e normal,build,features` (default features, **no** `--all-features`, dev edges excluded) to evidence §4.1 — command text + exit code + narration is not sufficient. | **3.0** | §4.1 contains the actual graph output and `rcs-filter/test-utils` is absent from the default production graph. |
| 4 | Relabel **NT2 §7** as a *partial, mixed-worktree non-reproduction* (two files substituted under the pre-fix checkout `714e1293`), explicitly **not** full old-checkout coverage. | **3.0** | §7 states the mixed-worktree boundary in its own words; no wording implies full old-checkout validation. |
| 5 | **Do NOT redo** the native `LimitExceeded` group or the CREATE-inactive-hit group without a specific, nameable trigger scenario. The building spot-checked `context.rs` (`fail_closed_screen`, `execute_best_transactions`) and `rcs_capture.rs` at `ae490d7f` and confirmed **no new code defect** in that scope. Change code only where a concrete trigger can be named. | **3.0** | Either no code change in those groups, or each change cites a named trigger scenario and a test that fails before / passes after. |
| 6 | **§4.2 OOM self-report**: either independently reconfirm that the two full-`cargo test` OOMs are an infrastructure artifact, or state plainly in the evidence doc that this was **not** reconfirmed this round. | **3.0** | §4.2 contains one of those two statements explicitly; no silent carry-over. |

Baseline for all six: current MR77 head **`ae490d7f`**. No force-push, no push to `main`, no second MR,
no merge/deploy/release/GitLab-approve, no new Jira issue, and **no cleanup** of `docs/superpowers`,
`.superpowers` or `superpowers-context.tar.gz`.

### Open Bus action for Stage 5.0

The building's dispatch **`starport-rcs-viking-focus-reczz28NDDzzYzst-r1` (Bus record `reczz28NDLogM6qj`,
correlation `reczz28NCcA0SK1N`)** was still `pending/queued` at the building's own readback. The next Stage 5.0
session that actually has `ok-okengine-lark-mcp` tools **must poll, consume (delivery=delivered, status=processing)
and reply to that exact record** — it must not be treated as already consumed by a round that only read it
through the Terran git record.

### Named limitation (unchanged from attempt 10)

`ok-okengine-lark-mcp` again exposed **zero tools** in this attempt's tool table, so the Lark Doc and the Bus
were not read or written live this round. No HTTP/REST fallback was attempted. This does **not** block the stage:
no new revision needed publishing, and the r11 decision evidence was read in full from the reviewer's own
committed Terran record plus the building's live `MEMORY.md`. A *future* new revision cannot be published or
submitted for review until those tools are restored.

### History recovery performed this attempt (nothing overwritten with older content)

The attempt-11 workspace arrived (CoW) with `specs/…-design.md` and this file missing, so both were restored from
the previous attempt directory (r11 state) and then extended. The **newest** stage-5 checkpoint
`superpowers-context.tar.gz` (sha256 `577038c04d59206dc1d81a8d7849d3d18103d2544c1457fd1ac4a7e69c6cb7a2`,
144438 bytes, rebuilt by Stage 5.0 attempt 18) was validated and extracted read-only; from it this workspace
recovered content that was absent here:

- `plans/2026-10-13-…-plan-r10.md`, `plans/2026-10-13-…-plan-r11.md`, `plans/2026-10-14-…-plan-r11.md`
- `specs/2026-10-13-rcs-native-transfer-mr77-r10-evidence.md`, `specs/2026-10-14-rcs-native-transfer-mr77-r11-evidence.md`
  (the r11 evidence document items 2/3/4/6 above refer to)
- `.superpowers/sdd/**`

The archive's own `lark-review-doc.md` is an **r10-header, block-level-detailed** variant. It was **not** used to
replace this r11 record (that would regress the revision); it is preserved verbatim as
`history/lark-review-doc-r10-detailed-from-stage5-archive.md`. The archive's
`specs/…-design.md` hashes identically to the restored one (`238cdfd2…1953` before this attempt's baseline-row
metadata correction), independently confirming the r11 design lineage. The older r4-era root archive
(`749ee0e9…ca44`, 34937 bytes) was kept as `superpowers-context-r4era-20261013.tar.gz`; the canonical root
`superpowers-context.tar.gz` is now the newest (`577038c0…b7a2`). The unit memory ledger was recovered from the
shared Terran branch only — never from any archive.
