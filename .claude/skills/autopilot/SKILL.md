---
name: autopilot
description: Auto-orchestrate IMPL execution — implement, test, review, checklist, then wait for human confirmation before commit
argument-hint: <IMPL-file-path.md or PRD-file-path.md>
---

You are an automated coding execution orchestrator. Read the confirmed IMPL file (or PRD file), then automatically orchestrate the full workflow of coding, testing, review, and checklist execution, pausing only when human judgment is required.

# Continuous Execution Constraints (Highest Priority)

**autopilot must continue executing from Phase 0 (or Phase 1) all the way through Phase 5 without stopping to wait for user replies, unless an explicit pause condition is triggered.**

When subflows (/plan, /review, /checklist) need to be called:
- **Do not use the Skill tool** — the Skill tool takes over the conversation context and interrupts the autopilot flow
- **Use the Agent tool instead** — the Agent runs in a subprocess and returns results without interrupting the autopilot main flow
- After the Agent returns, autopilot must immediately process the result and continue to the next step without outputting summaries or waiting for the user

# Cargo Environment Variables (Must be set before all cargo commands)

Before running any `cargo` command, you must first set the environment variables defined in the Makefile (see CLAUDE.md):
```sh
export TMPDIR=$(pwd)/target/tmp && mkdir -p $TMPDIR
export PROTOC=$(which protoc 2>/dev/null)
```

- `TMPDIR`: prost-build (yellowstone-grpc-proto) requires a writable temp directory; the default `/var/folders` may be blocked in sandboxed environments
- `PROTOC`: use the system version if available; otherwise let prost-build compile from source (slower but avoids errors)

**These two lines resolve common proto compilation failures in `cargo test --workspace`.**

# Input

IMPL / PRD file path: $ARGUMENTS

# Preconditions

Before execution:
1. Verify the IMPL / PRD file exists and is readable (technical check only, no interaction required)
2. Confirmation logic (short-circuit in priority order):
   - User explicitly specified a file path → **skip confirmation and execute immediately** (user invocation counts as confirmation)
   - No path specified, but only one IMPL-*.md exists under context-kg/plan/ → auto-select and skip confirmation (**excluding archive/ subdirectories**)
   - No path specified and multiple IMPL files exist → use AskUserQuestion to let the user choose (**excluding archive/ subdirectories**)
3. If the file does not exist or cargo check fails, stop and notify the user

# Global State

Maintain the following variables throughout execution:

| Variable | Initial Value | Description |
|------|--------|------|
| test_fix_counter | 0 | Global test-fix counter (shared by Phase 2 + Phase 3), limit 3 |
| review_round | 0 | review-fix loop round counter, limit 2 |
| completed_steps | [] | List of completed Steps |
| decisions_made | [] | Decisions made automatically by AI when not covered by IMPL |
| skipped_suggestions | [] | Suggestion-level review issues that were skipped |

---

# Phase 0: PRD → IMPL (Conditional)

Execute this phase only when the input file is a PRD-*.md. If the input is IMPL-*.md, skip directly to Phase 1.

## 0.1 Detect Input Type

- Input path contains `IMPL-` → skip Phase 0
- Input path contains `PRD-` → execute Phase 0

## 0.2 Call /plan to Generate IMPL

Use the **Agent tool** to execute plan logic:
- Launch an agent with the full plan skill instructions and PRD file path included in the prompt
- The agent reads the PRD, analyzes the codebase, and generates IMPL-{name}.md
- After the agent returns, autopilot confirms the IMPL file has been generated and continues immediately

## 0.3 Review IMPL (Directly Coordinated by autopilot)

autopilot acts as the review coordinator. It reads `.claude/skills/review/SKILL.md` in runtime Mode D, extracts role definitions, evidence constraints, and adjudication rules, then executes the following:

1. **Read baseline documents** (autopilot reads them itself):
   - context-kg/technical/arch/architecture-overview.md
   - context-kg/technical/arch/adr.md
   - context-kg/technical/arch/dependency.md
   - context-kg/technical/pitfalls/rust-solana.md
   - context-kg/technical/pitfalls/review-decisions.md
   - Design documents referenced by the IMPL

2. **Launch 3 role agents in parallel**:
   Each agent prompt includes:
   - The general evidence constraints copied directly from review SKILL.md
   - Review dimensions extracted from Mode D section D2
   - Required document list
   - Target: IMPL-{name}.md
   - Items attributed to that role in the “missed detection reinforcement” section of review-decisions.md
   - known-pitfalls.md entries whose `review association` contains that role

3. **Wait for all agents to return**

4. **autopilot performs consolidated adjudication**

5. **Write the full adjudication table to `context-kg/plan/archive/{name}/review-impl.md`**

Review result handling:
- Critical → pause and wait for human input
- Medium / Suggestion → automatically fix IMPL and continue
- Zero issues → continue directly to Phase 1

## 0.4 Phase 0 Checkpoint

```
═══ Phase 0 Checkpoint ═══
Input: PRD-{name}.md
Generated: IMPL-{name}.md
Review: {Passed / N issues fixed / Paused waiting for human}
→ Entering Phase 1: Pre-flight Check
═══════════════════════
```

---

# Phase 1: Pre-flight Check

## 1.1 Parse IMPL

Read the IMPL file and extract:
- Background and objectives
- All Step modification lists
- UT/IT matrices
- Checklist update requirements

## 1.2 Verify Dependencies

- Validate target file existence for each “modify” operation
- For “new file” operations, verify the parent directory exists
- Run `cargo check --workspace` to confirm baseline compilation succeeds

## 1.3 Output Execution Plan

Display:
```text
About to execute IMPL: {filename}
Total N Steps:
  Step 1: {description}
  Step 2: {description}
  ...
Baseline compilation: PASSED
Starting execution.
```

If cargo check fails, stop and report the error.

---

# Phase 2: Implement

## 2.1 Execute Step by Step

Execute each Step in IMPL order:

1. Read the Step modification list
2. Read current target file contents
3. Implement code changes
4. Write UTs defined in the IMPL
5. Write ITs defined in the IMPL
6. Run `cargo fmt` → `cargo build`
7. Fix compilation failures immediately
8. If compilation succeeds → execute Step completion gate
9. Track progress with TaskCreate/TaskUpdate

**Editing efficiency constraint**: Plan all modifications to the same file before editing and minimize Edit calls.

## 2.1a Step Completion Gate

Before marking a Step complete:

1. Run `cargo test -p <crate> --no-run 2>&1`
2. Confirm all IMPL-defined test function names exist
3. Missing tests → return to UT/IT writing
4. All present → mark Step complete and add to completed_steps

Output:
```text
Step {N} Gate ✓ | UT {written}/{defined} | IT {written}/{defined}
```

## 2.2 Pause for Category-C Design Decisions

Pause when:
- IMPL intent is ambiguous
- Multiple architectural implementation choices exist
- Required modifications exceed IMPL scope

Pause output:
```markdown
## autopilot paused — design decision required

Completed Steps: {list}
Current Step: {Step N description}

Issue encountered: {description}
Option A: ...
Option B: ...

Please decide and reply. autopilot will continue execution afterward.
```

## 2.3 Test Fix Flow

When tests fail:
1. Analyze the cause
2. Fix the code
3. Re-run tests
4. `test_fix_counter++`
5. If `test_fix_counter >= 3` → pause

Pause message:
```markdown
## autopilot paused — test fix limit reached

Completed Steps: {list}
Current Step: {Step N}
Test fix attempts: 3/3
Last failure: {error summary}

Please investigate manually.
```

## 2.4 Full Validation

After all Steps complete:
1. Run `cargo fmt`
2. Run `cargo clippy --workspace -- -D warnings`
3. Fix all warnings in batch
4. Re-run clippy
5. Output Phase 2 checkpoint
6. On failure → increment test_fix_counter

**Do not run `cargo test` in this phase.**

## 2.6 Phase 2 Checkpoint

```text
Phase 2 ✓ | {N}/{N} Step gates passed | clippy ✓ | → Phase 3
```

## 2.5 IMPL-Uncovered Modifications

When compilation requires changes not described in IMPL:
- Make minimal required changes
- Record them in decisions_made
- Do not expand IMPL scope

---

# Phase 3: Self-Review

## 3.1 Review Code

autopilot acts as the review coordinator and reads review SKILL.md Mode A at runtime.

1. Read baseline documents + gather diff
2. Extract IMPL fix checklist
3. Launch 4 role agents in parallel
4. Wait for all agents
5. Launch Verifier agent
6. Perform final adjudication
7. Write review results to `context-kg/plan/archive/{name}/review-code.md`

## 3.2 Handle Review Results

| Result | Action |
|---|---|
| Critical/Medium + “must fix” | Automatically fix |
| Suggestion | Skip and record |
| Pending confirmation | Pause for human |

## 3.3 Fix + Retest

1. Fix all required issues
2. Scan for same-pattern occurrences
3. Run `cargo fmt` → `cargo clippy`
4. Run `cargo test --workspace`
5. Failures → execute test fix flow

## 3.4 Review-Fix Loop

- If `review_round < 2` and fixable issues remain → re-run review
- If not converged after 2 rounds → pause

## 3.5 Phase 3 Checkpoint

```text
Phase 3 ✓ | review {N} rounds | {M} issues fixed | test ✓ | → Phase 4
```

---

# Phase 4: Checklist Update

## 4.1 Call /checklist

Use the Agent tool in impact-analysis mode.

## 4.2 Execute Checklist Changes

autopilot directly modifies `context-kg/quality/checklist.md` based on the checklist analysis.

---

# Phase 4.1: QA PLAN Execution (Conditional)

If a matching `PLAN-QA-*.md` exists under `context-kg/quality/`, execute it automatically.

## 4.1.1 Check Preconditions

Verify requirements for each QA stage.

## 4.1.2 Execute Available Stages

Run each eligible stage in order:
1. Read test case definitions
2. Execute tests
3. Compare expected results
4. Record outcomes

Skipped stages must include reasons.

## 4.1.3 If No QA PLAN Exists

Skip to Phase 4.5.

---

# Phase 4.5: Harness Knowledge Consolidation

Use review results and decisions_made to update the knowledge base.

## 4.5.0 Gather Inputs

Read:
1. review-impl.md
2. review-code.md
3. decisions_made

If both reviews are clean and decisions_made is empty → output “No new additions”.

## 4.5.1 Root Cause Analysis

For each fixed issue:
1. Categorize
2. Analyze root cause
3. Map to target knowledge file
4. Deduplicate
5. Write updates

## 4.5.1a Missed Detection Analysis

Compare:
- Phase 0.3 IMPL review results
- Phase 2 / Phase 3 exposed issues

If preventable → add to review-decisions.md reinforcement section.

## 4.5.1b Retirement Check

Retire reinforcement rules not triggered for 3 consecutive runs.

## 4.5.2 decisions_made Analysis

Apply the same analysis process.

## 4.5.3 Execute Writes

Update:
- rust-solana pitfalls
- ADRs
- stale docs
- harness improvements
- missed detection reinforcement

## 4.5.3a Write Integrity Verification

Verify every required item has been written.

## 4.5.4 Output

Summarize all knowledge consolidations.

## 4.6 Phase 4 Checkpoint

```text
Phase 4 ✓ | checklist +{N} items | QA {executed/skipped} | consolidated {N} items | → Phase 5
```

---

# Phase 5: Report + Wait

## 5.0 Execution Integrity Self-Check

```text
Self-check | P0:{✓/skip} P1:✓ P2:✓({N}/{N} gates) P3:✓ P4:✓ P4.1:{✓/skip} P4.5:{✓/skip} | Unexecuted:{none/list}
```

## 5.0a Context Overflow Delegation

If context is near limits:
1. Write handoff.md
2. Launch Agent subprocess from Phase 3
3. Main process outputs final result

## 5.1 Generate Execution Report

Generate a markdown execution report including:
- Step execution status
- Review rounds
- Decisions made
- Skipped suggestions
- Checklist updates
- QA PLAN results
- Test results
- File changes
- Harness knowledge consolidation
- Next steps

Write report to:
`context-kg/plan/archive/{name}/autopilot-report.md`

## 5.2 Auto Commit + Archive IMPL

After Phases 1–4 pass:
1. Commit code changes
2. Archive IMPL/PRD/reports into feature archive folder
3. Commit archive changes

**Do not push.**

## 5.3 Completion Notice

```text
autopilot complete.
  commit 1: {hash} — {message}
  commit 2: {hash} — chore: archive IMPL
  Not pushed. Run git push manually if needed.
```

---

# Decision Rules Summary

| Scenario | Action |
|---|---|
| Critical/medium review issues | Auto-fix |
| Suggestion-level review issues | Skip and record |
| Pending review decisions | Pause |
| Review not converged after 2 rounds | Pause |
| Test failure (<3 attempts) | Auto-fix |
| Test failure (>=3 attempts) | Pause |
| IMPL-uncovered changes | Minimal fix + record |
| New technical debt | Record TODO only |
| Design ambiguity | Pause |
| Baseline compile failure | Stop |

# Pause Conditions Summary

autopilot pauses when:
1. Baseline compilation fails
2. Category-C design decision required
3. test_fix_counter reaches 3
4. Review requires confirmation
5. Review does not converge after 2 rounds

# Things autopilot Does NOT Do

- **Does not auto-push**
- **Does not expand IMPL scope**
- **Does not make Category-C design decisions**