# JIT Bench Timing Instrumentation — Design

**Date**: 2026-06-07
**Status**: Approved (pending user review of this written spec)
**Author**: jie.zhang1@okg.com
**Schema version**: 1

---

## 1. Background

xlayer-reth currently has two pieces of timing instrumentation on the bench branches:

- `op-reth/crates/payload/src/builder.rs` (branch `xl/jit-bench-timing`) — emits a per-block `xlayer::bench_timing` log with four phases: `pre_exec_us`, `seq_txs_us`, `pool_exec_us`, `finish_us`, `total_us`.
- `xlayer-reth/crates/builder/src/evm_jit/executor.rs` (branch `xl/jit-timing`) — spawns a Tokio task that emits a `xlayer::jit::stats` snapshot every 1000 ms with JIT cache counters.

These two logs measure **chain block CPU time** and **JIT engine behavior**. Both diffs are uncommitted in their working trees.

The bench driver scripts (`run-bench-aot.sh`, `run-paid-Nx3.sh`, `setup-golden.sh`) and the polycli-based SA-Benchmark workload sit **outside** these measurements. As the handoff doc `JIT-BENCH-HANDOFF.md` §9 notes:

> pool_exec + finish 占 chain block 内部 99.9%,但 chain block 时间只占 polycli wall-clock 的 ~10%。

So ~90% of polycli wall-clock — cp-golden, node startup, polycli warmup, txpool drain, sleep, RPC round-trips, block-time idle — is currently a black box. The existing `parse-bench-timing.py` also has gaps: it expects `n_pool_txs` / `n_seq_txs` fields the emitter doesn't send, only keeps the last JIT snapshot (losing the warmup-vs-real attribution), and doesn't apply the heavy-block filter the handoff doc requires.

## 2. Goals

Cover the following layers of "where does the full bench-cycle wall-clock go":

| # | Coverage layer | Status |
|---|---|---|
| ① | Pipeline wall-clock distribution (across N rounds × 3 modes) | **In scope** |
| ② | Per-run wall-clock distribution (cp / start / warmup / drain / real / stop) | **In scope** |
| ③ | polycli end-to-end view (Final TPS + delta vs chain CPU) | **In scope (lightweight)** |
| ④ | Per-block chain CPU 4-phase breakdown | Already covered; +`n_seq_txs`/`n_pool_txs` |
| ⑤ | EVM sub-distribution (interpret / JIT / precompile) | **Out of scope** |
| ⑥ | JIT engine behavior (compile / lookup) aligned to bench window | **In scope** |

Non-goals: EVM call-level instrumentation (⑤); storage-heavy or flashblocks workloads; cross-machine NTP-aware timing; historical log compatibility beyond the existing `--label` mode of the parser.

## 3. Approach

**Approach 1 — Minimum-invasion (shell + parser primary, 2 small Rust info! calls).** Chosen over a Python orchestrator rewrite (Approach 2) because shell precision at ~10ms is sufficient for the wall-clock layer, and the current shell structure is well-known to the team. Approach 3 (all-Rust event emission) was rejected because shell-only phases (`cp` golden state, `pkill`) are invisible to the node — and those phases are non-trivial on slow disks.

## 4. Architecture & Data Flow

Three log sources per run, merged offline by a rewritten parser:

```
                           ┌──────────────────────────────────┐
                           │  run-bench-aot.sh — one run      │
                           └──────────────────────────────────┘
                                          │
       ┌──────────────────────────────────┼──────────────────────────────────┐
       │ source A:                        │ source B:                        │ source C:
       │ shell phase tslog                │ node log (existing + 2 new logs) │ polycli output (existing)
       │ aot-cycle-${MODE}-${TS}.tslog    │ aot-node-${MODE}-${TS}.log       │ aot-result-${MODE}-${TS}.out
       │                                  │                                  │
       │ one line per event:              │ - bench_timing (per block)       │ - Final TPS
       │   ts=... phase=cp_start          │ - jit::stats (every 1s)          │ - tps= (per second)
       │   ts=... phase=cp_end            │ - aot_store_opened (NEW)         │ - numErrors
       │   ts=... phase=warmup_end        │                                  │
       │              block=42            │                                  │
       └──────────────────────────────────┴──────────────────────────────────┘
                                          │
                                          ▼
                            ┌─────────────────────────────┐
                            │ parse-bench-timing.py       │
                            │ (rewritten)                 │
                            │                             │
                            │ - read & merge 3 sources    │
                            │ - slice by block number     │
                            │ - filter heavy blocks       │
                            │ - compute phase ratios      │
                            └─────────────────────────────┘
                                  │                │
              ┌───────────────────┘                └─────────────────┐
              ▼                                                       ▼
   Terminal: 4 tables                                       JSON: cycle-timing-${MODE}-${TS}.json
   1. Per-run wall-clock by phase (11 rows)                 (see Section 5)
   2. Chain block phases (real window, filtered)
   3. JIT periodic stats delta (real window)
   4. Final TPS + Effective chain TPS
```

Key design choices:

1. **shell tslog decoupled from node log.** Shell records only its own phase boundaries. Parser joins via block number, not wall-clock timestamps — immune to clock skew or sleep precision drift.
2. **Bench window cut by block number, not timestamps.** Shell calls `eth_blockNumber` at warmup_start, warmup_end, real_bench_start, real_bench_end. The block-number sentinels travel with the run regardless of when each log file is parsed.
3. **All new data is additive.** No existing log line is modified or removed; rollback at any step is a single `git reset`.

## 5. Components

### 5.1 Shell

**`run-bench-aot.sh`** — add ~50 lines, no deletions.

Introduce a `ts_log` helper at the top of the script:

```bash
TSLOG="$RESULT_DIR/aot-cycle-${MODE}-${TIMESTAMP}.tslog"
ts_log() {
  # phase=$1, extra kv pairs $2..
  local ts
  ts=$(python3 -c 'import time; print(f"{time.time():.6f}")')
  echo "ts=$ts phase=$1 ${@:2}" >> "$TSLOG"
}
```

Then bracket every existing phase boundary with `ts_log <name>_start` and `ts_log <name>_end`. Phases instrumented (9 phases + the synthetic `total_run_us` field in the JSON output = 10 wall-clock fields):

1. `cleanup` — pkill + rm -rf
2. `cp_golden` — `cp -R golden-state jit-data`
3. `node_start` — process spawn until first log
4. `rpc_wait` — `wait_for_rpc` polling
5. `warmup_polycli` — 2000 UOP polycli loadtest
6. `drain_txpool` — drain polling
7. `sleep5` — fixed sleep
8. `real_bench` — `./2-bench.sh` 50k UOP
9. `stop_node` — kill + sleep

Two more conditional phases exist in `run-bench-aot.sh` but are skipped in the hot-path bench (they only fire on first-time AOT prewarmup or in the absence of a golden-state snapshot):
- `aot_prewarmup` — only `MODE=aot` with `AOT_PREWARMUP=1`
- `deploy_contracts` — only if `golden-state/` doesn't exist (the normal flow uses `cp -R golden-state`)

When skipped, these fields are omitted from the JSON entirely (not null).

Block-number sentinels (4 markers, fetched via `curl ... eth_blockNumber`):

- `warmup_start_block` (before `warmup_polycli`)
- `warmup_end_block` (after `warmup_polycli`)
- `real_start_block` (before `./2-bench.sh`)
- `real_end_block` (after `./2-bench.sh`)

A `safe_block_number` helper retries 3× and on failure writes `block=unknown`; the parser falls back to wall-clock slicing with a warning.

**`run-paid-Nx3.sh`** — add ~10 lines. Outer-loop tslog `paid-Nx3-cycle.tslog` records each `round/mode` boundary and the `cool_down` start/end. This is independent of per-run tslogs — used for ① pipeline-level aggregation.

**`setup-golden.sh` / `run.sh`** — unchanged.

### 5.2 Rust

**op-reth `crates/payload/src/builder.rs`** (~5 lines) — branch `xl/jit-bench-timing`.

Snapshot `info.cumulative_tx_count` at the seq→pool boundary, emit two new fields:

```rust
let mut info = ctx.execute_sequencer_transactions(&mut builder)?;
let n_seq_txs = info.cumulative_tx_count;  // NEW: snapshot
let t2 = Instant::now();

// ... pool_exec ...
let t3 = Instant::now();
let n_pool_txs = info.cumulative_tx_count - n_seq_txs;  // NEW: compute

info!(
    target: "xlayer::bench_timing",
    block = ..., n_total_txs = ...,
    n_seq_txs = n_seq_txs,           // NEW
    n_pool_txs = n_pool_txs,         // NEW
    pre_exec_us = ..., seq_txs_us = ...,
    pool_exec_us = ..., finish_us = ..., total_us = ...,
    "block_phase_breakdown"
);
```

**xlayer-reth `crates/builder/src/evm_jit/aot_loader.rs`** (~3 lines) — branch `xl/jit-timing`.

After `AotStore::open` (or whatever the equivalent entry point is — to be confirmed during implementation by reading the file), emit:

```rust
let dlopen_us = (Instant::now() - t_start).as_micros() as u64;
info!(
    target: "xlayer::aot",
    dlopen_us = dlopen_us,
    n_dylib_loaded = self.loaded.len(),
    n_manifests = self.manifests.len(),
    "aot_store_opened"
);
```

**If no clean entry point exists in `aot_loader.rs`**, fall back to wrapping the call site in `evm_jit/executor.rs::build_evm` with a timer.

**`executor.rs::spawn_periodic_jit_stats`** — unchanged. Parser consumes the full time series instead of just the last snapshot.

### 5.3 Parser — `scripts/parse-bench-timing.py` (rewrite)

Four stages:

1. **Read `.tslog`** → list of `{ts, phase, extra_kv}` events.
2. **Read node log** → grep `block_phase_breakdown`, `jit_periodic_stats`, `aot_store_opened`.
3. **Read polycli `.out`** → grep `Final TPS:` + `numErrors`.
4. **Merge**:
   - Slice `block_phase_breakdown` rows by `[real_start_block, real_end_block]` using the `block` field on each emitted line.
   - Default filter: drop rows where `n_total_txs < 100`. Configurable via `--min-block-txs N`.
   - JIT stats window alignment:
     - The shell `ts_log` for `real_bench_start` records both the wall-clock ts (`ts=...`) and the block number (`block=N`). Call that wall-clock `T_real_start`. Same for `T_real_end`.
     - `jit_periodic_stats` lines in the node log carry tracing's default RFC-3339 timestamp prefix; parser parses that as `T_snapshot`.
     - `start_snapshot` = the first `jit_periodic_stats` line with `T_snapshot >= T_real_start`; `end_snapshot` = the last with `T_snapshot <= T_real_end`. Delta = `end - start`.
     - Same logic for the warmup window, using `T_warmup_start` and `T_warmup_end`.
     - If tracing timestamp format proves unparseable during implementation, fallback: re-emit the periodic stats with `info!(target: "xlayer::jit::stats", ts_unix = ..., ...)` carrying an explicit `ts_unix` field. This is a 2-line change to `executor.rs` and is the only conditional Rust diff in the plan.
   - Compute aggregates (p50/p95/mean per phase, per-tx, per-Mgas).
5. **Emit** terminal tables + `cycle-timing-${MODE}-${TS}.json`.

New CLI:

```bash
# Single run → table + JSON
parse-bench-timing.py --run-dir bench-results/ --mode aot --timestamp 20260607_143012

# Backward-compatible multi-mode comparison (old usage still works)
parse-bench-timing.py --label nojit X.log --label aot Y.log

# N×3 aggregation over a paid-Nx3 directory
parse-bench-timing.py --aggregate bench-results/paid-3x3-20260607_143012/
```

## 6. JSON Schema (per-run output)

```jsonc
{
  "schema_version": 1,
  "meta": {
    "mode": "aot",                        // nojit | jit_cold | aot
    "round": 2,                           // present in Nx3 driver context, else null
    "timestamp": "20260607_143012",
    "git": {
      "xlayer_reth_sha": "abc1234",
      "op_reth_sha": "def5678",
      "branch_xlayer": "xl/jit-timing",
      "branch_op_reth": "xl/jit-bench-timing"
    },
    "env": {
      "XLAYER_JIT_ENABLED": "1",
      "XLAYER_AOT_DIR": "/.../aot-cache",
      "BLOCK_TIME": "1s"
    },
    "warnings": []                        // e.g. ["tslog_partial: run_end missing"]
  },

  "wall_clock_by_phase": {                // ① + ② — answers "where does the run go"
    "cleanup_us":       2010234,
    "cp_golden_us":     8541022,
    "node_start_us":    1234500,
    "rpc_wait_us":       987600,
    "warmup_polycli_us": 45203440,
    "drain_txpool_us":   8210000,
    "sleep5_us":         5001230,
    "real_bench_us":    187432100,
    "stop_node_us":      5012003,
    "total_run_us":     263731129         // run_start → run_end (sanity vs sum)
  },

  "bench_window": {
    "warmup_start_block": 8,
    "warmup_end_block":   24,
    "real_start_block":   42,
    "real_end_block":     287,
    "n_real_blocks":      245,
    "n_blocks_filtered_light": 18         // n_total_txs < 100 filtered out
  },

  "chain_blocks_real": {                  // ④ — real window only, light blocks filtered
    "n_blocks": 227,
    "phase_us": {
      "pre_exec":  { "p50": 350,    "p95": 820,    "mean": 410   },
      "seq_txs":   { "p50": 1200,   "p95": 2100,   "mean": 1380  },
      "pool_exec": { "p50": 78400,  "p95": 145000, "mean": 88300 },
      "finish":    { "p50": 12400,  "p95": 28000,  "mean": 14200 },
      "total":     { "p50": 92500,  "p95": 168000, "mean": 104290}
    },
    "tx_counts": {
      "n_seq_txs":   { "avg": 1.0,   "max": 1   },
      "n_pool_txs":  { "avg": 211.3, "max": 487 },
      "n_total_txs": { "avg": 212.3, "max": 488 }
    },
    "per_tx_pool_us":    372,             // Σpool_exec_us / Σn_pool_txs
    "per_mgas_total_us": 184              // Σtotal_us / (Σgas_used / 1e6)
  },

  "jit_stats_delta": {                    // ⑥ — real window only
    "real_window_seconds":  187,
    "compile_ok_delta":     0,            // typically 0 — all hot contracts compiled during warmup
    "compile_ok_total":     4,
    "lookup_hits_delta":    48230,
    "lookup_misses_delta":  18,
    "lookup_hit_rate":      0.99963,
    "evictions_delta":      0,
    "code_bytes_final":     2147834,
    "compile_ok_warmup":    4,
    "lookup_hits_warmup":   3210
  },

  "aot": {                                // AOT mode only; null for nojit / jit_cold
    "dlopen_us": 142000,
    "n_dylib_loaded": 4,
    "n_manifests": 4
  },

  "tps": {
    "polycli_final": 287.3,
    "polycli_errors": 0,
    "effective_chain": 1198.2,
    "per_tx_pool_us": 372,
    "wall_clock_bench_ratio": 0.0857      // Σ(chain block total_us) / real_bench_us
                                          // → "chain CPU as fraction of polycli wall-clock"
  }
}
```

## 7. Error Handling & Boundary Cases

### 7.1 Missing or corrupt sources

| Failure | Behavior |
|---|---|
| `.tslog` missing (script crashed early) | `wall_clock_by_phase: null` + `meta.warnings: ["tslog_missing"]`; other sections still emitted |
| `.tslog` partial (no `run_end`) | Compute what's possible; warning `"tslog_partial: run_end missing"` |
| Node log has no `block_phase_breakdown` (e.g. running off the wrong branch) | `chain_blocks_real: null` + warning `"no_bench_timing_lines"` |
| `aot_store_opened` missing in AOT mode | Warning; non-AOT modes simply omit the `aot` block |
| polycli `.out` unparseable for Final TPS | `tps.polycli_final: null`; effective_chain still derived |
| `eth_blockNumber` fails (RPC down) | `safe_block_number` retries 3× then writes `block=unknown`; parser falls back to wall-clock slicing with `meta.warnings: ["window_by_wallclock_fallback"]` |

**Core principle**: no source failure causes parser crash. Missing fields become `null`, warnings are recorded in `meta.warnings[]`, and terminal tables show `n/a`.

### 7.2 Timestamp / block-number degraded paths

- macOS BSD `date` lacks nanoseconds — use `python3 -c 'import time; print(f"{time.time():.6f}")'` everywhere. Effective precision ~ms, fine for the 100ms-class wall-clock answers we want.
- Block numbers reset to 0 across mode switches (each run is a fresh `--dev` node) — `mode=$MODE` is recorded in tslog and the parser scopes block numbers to a single run.
- Cross-mode block numbers are never compared.

### 7.3 Compatibility

- Existing `parse-bench-timing.py --label nojit X.log --label aot Y.log` continues to work; the old code path is preserved as a legacy mode.
- `block_phase_breakdown` log line format gains 2 fields (`n_seq_txs`, `n_pool_txs`) but keeps all existing fields. Old logs parsed with the new parser yield `null` for the new fields.
- JSON output carries `schema_version: 1` for forward compatibility.

### 7.4 Out of scope failure modes

Explicitly not handled by this plan:

- Cross-machine clock skew (single-machine bench)
- Concurrent writes to a single `.tslog` (each run has its own file)
- Node panic mid-bench: tslog will lack `run_end`; parser flags as partial
- AOT cache partially populated (<4 dylib): doesn't affect timing measurement itself, parser doesn't enforce
- polycli stuck while chain produces empty blocks: heavy-block filter (`n_total_txs >= 100`) excludes them naturally

## 8. Testing & Acceptance

### 8.1 Rust changes

No new unit tests — both diffs are pure instrumentation. Verification by:

- **op-reth `n_seq_txs` / `n_pool_txs`**: `n_seq + n_pool == n_total` must hold in emitted logs (parser checks and warns on violation).
- **`aot_store_opened`**: grep node log after one AOT run; must have exactly 1 line with all fields populated.

### 8.2 Shell changes

Run `MODE=nojit ./run-bench-aot.sh` once to validate:

- `.tslog` exists, ~20 lines
- Every line awk-parseable (no quote/escape issues)
- Timestamps strictly monotonic
- Every `<phase>_start` has a matching `<phase>_end` (or next phase start)

For `run-paid-Nx3.sh`: `ROUNDS=1` validates round/mode boundary markers are present.

### 8.3 Parser

Three fixture directories under `scripts/tests/fixtures/`:

1. **complete-run/** — full normal tslog + node log + .out; assert JSON fields populated and within sane ranges
2. **partial-tslog/** — tslog missing `run_end`; assert no crash, correct warning, other sections still emitted
3. **no-bench-timing/** — node log without `block_phase_breakdown` (simulates wrong-branch node); assert `chain_blocks_real: null`, no crash

```bash
python3 -m pytest scripts/tests/test_parser.py -v
```

### 8.4 End-to-end acceptance (mandatory single run)

Run `ROUNDS=1 ./run-paid-Nx3.sh` (3 modes × 1 round, ≈15–25 min) and verify:

| # | Check | Pass condition |
|---|---|---|
| 1 | 3 `.tslog` files exist | ✓ |
| 2 | 3 `cycle-timing-${MODE}-${TS}.json` exist | ✓ |
| 3 | Each JSON `wall_clock_by_phase`: all 10 fields non-null (9 phase + `total_run_us`); conditional `aot_prewarmup_us` / `deploy_contracts_us` may be absent | ✓ |
| 4 | Each JSON `bench_window.real_start_block < real_end_block` | ✓ |
| 5 | `chain_blocks_real.n_blocks >= 20` (50k UOP yields ≥20 blocks) | ✓ |
| 6 | Cross-mode `chain_blocks_real.phase_us.total.mean`: nojit > aot expected on JIT-friendly workloads | ⚠ informational only — workload is precompile-heavy so JIT may not win cleanly; never a plan blocker |
| 7 | AOT mode JSON `aot.dlopen_us > 0` | ✓ |
| 8 | AOT mode `aot.n_dylib_loaded == 4` | ✓ |
| 9 | JIT mode `jit_stats_delta.lookup_hits_delta > chain_blocks_real.n_blocks` | ✓ |
| 10 | `Σ wall_clock_by_phase ≈ total_run_us` (within 5%) — conservation | ✓ |
| 11 | Terminal 4 tables format clean, no misalignment | ✓ |

Failing any of 1–5 or 7–11 blocks plan completion.

### 8.5 Performance overhead

The instrumentation itself must not measurably slow down bench:

- Shell `ts_log`: ~20 lines × ~20-30ms `python3` fork = ~0.5s/run. Acceptable (real bench ~3 min). Fallback if measurable: bash 5 `EPOCHREALTIME` shell var (no fork).
- Rust info!: ns-class, negligible.

Verification: run nojit mode once pre-change and once post-change, compare `chain_blocks_real.phase_us.total.mean`. Difference must be < 2% (within noise).

### 8.6 Out of acceptance scope

- Multi-round (ROUNDS≥3) statistical stability — user runs longer batches post-plan
- Compatibility with historical `bench-results/` data — old logs use the legacy `--label` parser mode
- Storage-heavy / flashblocks workloads — handoff doc §11 options B/C, not in this plan

## 9. Implementation Plan

### 9.1 Order of steps (low risk → higher risk, each step independently committable)

| Step | Scope | Risk |
|---|---|---|
| 1 | Parser rewrite + fixture tests | low — offline only |
| 2 | op-reth: add `n_seq_txs` / `n_pool_txs` (5-line diff) | low |
| 3 | xlayer-reth: `aot_store_opened` info! (3-line diff) | low |
| 4a | shell: `ts_log` helper + phase boundary calls | med — shell logic |
| 4b | shell: warmup/real block sentinels | med |
| 5 | `run-paid-Nx3.sh` outer-loop tslog | low |
| 6 | End-to-end acceptance run (Section 8.4) | gate |
| 7 | `JIT-BENCH-HANDOFF.md` doc update | low |

### 9.2 Commits

**op-reth submodule on `xl/jit-bench-timing`** — 2 commits:

1. `feat(bench): emit per-block phase breakdown for OpBuilder::build` (existing uncommitted work — commit first)
2. `feat(bench): emit n_seq_txs and n_pool_txs in block_phase_breakdown`

**xlayer-reth on `xl/jit-timing`** — 7-8 commits:

1. `feat(bench): periodic JIT stats log` (existing uncommitted `executor.rs` change)
2. **bump op-reth submodule** to xl/jit-bench-timing HEAD
3. `feat(bench): emit aot_store_opened with dlopen_us`
4. `feat(bench): add ts_log helper + phase boundaries to run-bench-aot.sh`
5. `feat(bench): emit bench window block markers (warmup/real)`
6. `feat(bench): outer-loop tslog in run-paid-Nx3.sh`
7. `feat(bench): rewrite parse-bench-timing.py to emit per-run JSON + cycle table`
8. `docs(bench): update JIT-BENCH-HANDOFF.md with new instrumentation`

### 9.3 Push strategy

Push order: op-reth submodule **first**, then xlayer-reth (so the submodule pointer in xlayer-reth's commit 2 resolves).

Pre-push diff check on both branches:

```bash
git log <branch> --not origin/<branch> --stat
```

Confirm no `.env`, key, token, or large binary leaks. gitlab → github mirror handles GitHub propagation automatically (no separate push needed).

### 9.4 Rollback

Each step is an independent commit. Any step's failure → `git reset --hard HEAD~1`. End-to-end acceptance (Step 6) failure: diagnose by source and reset only the offending commit. AOT cache (`aot-cache/`) is data, not git-tracked — never touched by this plan.

### 9.5 Plan completion criteria

Plan is complete only when **all** of:

1. Both branches pushed to gitlab with all commits.
2. Section 8.4 acceptance: 11/11 pass (soft check #6 may be a warning).
3. Section 8.5 performance: < 2% overhead vs pre-change.
4. `JIT-BENCH-HANDOFF.md` updated with a new "Instrumentation reference" section explaining where each metric lives.
5. One `parse-bench-timing.py --run-dir <latest>` produces 4 terminal tables + a JSON, manually reviewed for sane field values.

## 10. Open Questions / Future Work

- **EVM sub-distribution (⑤)**: out of this plan but a natural next step — would attribute `pool_exec_us` to interpret vs JIT-call vs precompile, answering "is JIT pinned by ECDSA precompile time?".
- **polycli sender-side pprof**: `2-bench.sh:197` hints at `:6060/debug/pprof/profile`. Worth one experiment after this plan to crack open the ~90% wall-clock that's currently outside chain. Lightweight follow-up.
- **AOT compile-latency histogram**: current `compilations_succeeded` is a counter, not a distribution. If a single contract takes seconds to LLVM-compile, that's invisible. Out of scope here.
- **Mode-order rotation**: handoff doc §11 option A. Independent of this plan but the new tslog makes the rotation easier to verify in aggregate.
