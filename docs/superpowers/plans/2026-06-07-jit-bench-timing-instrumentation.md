# JIT Bench Timing Instrumentation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add full-cycle bench wall-clock instrumentation (cp golden, node start, warmup, drain, real bench, stop) plus per-block phase splits and JIT window-aligned stats. Output per-run JSON + 4 terminal tables. Cover layers ①②③⑥ from the spec.

**Architecture:** Three log sources (shell `.tslog`, node log, polycli `.out`) merged offline by a rewritten `parse-bench-timing.py`. Bench window cut by block number (immune to clock skew). All new data is additive — no existing log line modified.

**Tech Stack:** Bash 5 + Python 3 (stdlib only — uses `unittest`, no pytest dep) + Rust (revm/op-reth/reth fork). 2 small Rust diffs (~5 lines + ~3 lines), ~60 lines shell, ~200 lines Python.

**Spec reference:** `docs/superpowers/specs/2026-06-07-jit-bench-timing-instrumentation-design.md`

---

## Repo paths (referenced throughout)

- `XLAYER_RETH = /Users/jiezhang/meili/jie.zhang1_dacs_at_okg.com/114/Documents/repository/xlayer-reth`
- `OP_RETH = $XLAYER_RETH/deps/optimism/rust/op-reth` (submodule, branch `xl/jit-bench-timing`)
- `SA_BENCH = /Users/jiezhang/meili/jie.zhang1_dacs_at_okg.com/114/Documents/SA-Benchmark` (read-only in this plan)
- xlayer-reth branch: `xl/jit-timing`
- op-reth branch: `xl/jit-bench-timing`

All `cd` commands use absolute paths to keep the working dir consistent.

---

## Phase A — Commit existing uncommitted timing work

### Task 1: Commit existing op-reth phase timing

The 4-segment `OpBuilder::build` timing diff already exists in the op-reth working tree but is uncommitted. Commit it as the baseline before adding new fields.

**Files:**
- Modify: `$OP_RETH/crates/payload/src/builder.rs` (already modified in working tree)

- [ ] **Step 1: Verify the working-tree diff is intact**

```bash
cd /Users/jiezhang/meili/jie.zhang1_dacs_at_okg.com/114/Documents/repository/xlayer-reth/deps/optimism/rust/op-reth
git status --short crates/payload/src/builder.rs
git diff crates/payload/src/builder.rs | head -100
```
Expected: file shown as modified (`M`), diff shows `let t0 = Instant::now()` and `xlayer::bench_timing` info!.

- [ ] **Step 2: Confirm current branch + base**

```bash
cd /Users/jiezhang/meili/jie.zhang1_dacs_at_okg.com/114/Documents/repository/xlayer-reth/deps/optimism/rust/op-reth
git branch --show-current
git log --oneline -3
```
Expected: branch `xl/jit-bench-timing`.

- [ ] **Step 3: Stage and commit**

```bash
cd /Users/jiezhang/meili/jie.zhang1_dacs_at_okg.com/114/Documents/repository/xlayer-reth/deps/optimism/rust/op-reth
git add crates/payload/src/builder.rs
git commit -m "$(cat <<'EOF'
feat(bench): emit per-block phase breakdown for OpBuilder::build

Adds 4-phase timing (pre_exec / seq_txs / pool_exec / finish) plus
n_total_txs + gas_used as a single xlayer::bench_timing info! line
per built block. Bench-only branch; no impact on prod build path.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

- [ ] **Step 4: Verify**

```bash
cd /Users/jiezhang/meili/jie.zhang1_dacs_at_okg.com/114/Documents/repository/xlayer-reth/deps/optimism/rust/op-reth
git status --short crates/payload/src/builder.rs
git log --oneline -1
```
Expected: file is clean (no `M`); HEAD has the new commit.

---

### Task 2: Commit existing xlayer-reth periodic JIT stats

The `spawn_periodic_jit_stats` Tokio task in `executor.rs` is uncommitted. Commit as-is.

**Files:**
- Modify: `$XLAYER_RETH/crates/builder/src/evm_jit/executor.rs` (already modified in working tree)

- [ ] **Step 1: Verify diff is intact**

```bash
cd /Users/jiezhang/meili/jie.zhang1_dacs_at_okg.com/114/Documents/repository/xlayer-reth
git status --short crates/builder/src/evm_jit/executor.rs
git diff crates/builder/src/evm_jit/executor.rs | head -60
```
Expected: file modified (`M`); diff shows `spawn_periodic_jit_stats` call + function definition.

- [ ] **Step 2: Verify submodule pointer is also modified**

```bash
cd /Users/jiezhang/meili/jie.zhang1_dacs_at_okg.com/114/Documents/repository/xlayer-reth
git status --short
```
Expected output includes both:
```
 M crates/builder/src/evm_jit/executor.rs
 M deps/optimism
```
The submodule pointer was modified by Task 1's commit; commit that here too if not already on a clean SHA.

- [ ] **Step 3: Stage executor.rs only (NOT the submodule pointer yet — that's Task 9)**

```bash
cd /Users/jiezhang/meili/jie.zhang1_dacs_at_okg.com/114/Documents/repository/xlayer-reth
git add crates/builder/src/evm_jit/executor.rs
git commit -m "$(cat <<'EOF'
feat(bench): periodic JIT stats log

Spawn a tokio task that emits xlayer::jit::stats every
XLAYER_JIT_STATS_INTERVAL_MS (default 1000 ms) with a snapshot of
JitBackend counters (resident, compile_ok, lookup_hits, ...).
Interval=0 disables.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

- [ ] **Step 4: Verify**

```bash
cd /Users/jiezhang/meili/jie.zhang1_dacs_at_okg.com/114/Documents/repository/xlayer-reth
git log --oneline -1
git status --short
```
Expected: HEAD is the new commit; `executor.rs` clean; `deps/optimism` still shows `M` (we'll commit that in Task 9 after bumping submodule HEAD).

---

## Phase B — Parser rewrite (offline TDD, no node runs needed)

### Task 3: Parser test scaffolding + fixture data

Create `scripts/tests/` with `unittest`-based test runner and three fixture directories (complete-run, partial-tslog, no-bench-timing).

**Files:**
- Create: `$XLAYER_RETH/scripts/tests/__init__.py` (empty)
- Create: `$XLAYER_RETH/scripts/tests/test_parser.py`
- Create: `$XLAYER_RETH/scripts/tests/fixtures/complete-run/aot-cycle-aot-20260601_120000.tslog`
- Create: `$XLAYER_RETH/scripts/tests/fixtures/complete-run/aot-node-aot-20260601_120000.log`
- Create: `$XLAYER_RETH/scripts/tests/fixtures/complete-run/aot-result-aot-20260601_120000.out`
- Create: `$XLAYER_RETH/scripts/tests/fixtures/partial-tslog/aot-cycle-nojit-20260601_120100.tslog`
- Create: `$XLAYER_RETH/scripts/tests/fixtures/no-bench-timing/aot-cycle-nojit-20260601_120200.tslog`
- Create: `$XLAYER_RETH/scripts/tests/fixtures/no-bench-timing/aot-node-nojit-20260601_120200.log`

- [ ] **Step 1: Create directory layout**

```bash
mkdir -p /Users/jiezhang/meili/jie.zhang1_dacs_at_okg.com/114/Documents/repository/xlayer-reth/scripts/tests/fixtures/complete-run
mkdir -p /Users/jiezhang/meili/jie.zhang1_dacs_at_okg.com/114/Documents/repository/xlayer-reth/scripts/tests/fixtures/partial-tslog
mkdir -p /Users/jiezhang/meili/jie.zhang1_dacs_at_okg.com/114/Documents/repository/xlayer-reth/scripts/tests/fixtures/no-bench-timing
touch /Users/jiezhang/meili/jie.zhang1_dacs_at_okg.com/114/Documents/repository/xlayer-reth/scripts/tests/__init__.py
```

- [ ] **Step 2: Write `complete-run/aot-cycle-aot-20260601_120000.tslog`** (synthetic but realistic)

```
ts=1717228800.000000 phase=run_start mode=aot round=1
ts=1717228800.000123 phase=cleanup_start
ts=1717228802.010357 phase=cleanup_end
ts=1717228802.010500 phase=cp_golden_start
ts=1717228810.551522 phase=cp_golden_end
ts=1717228810.551700 phase=node_start
ts=1717228811.786200 phase=rpc_wait_start
ts=1717228812.773800 phase=rpc_ready
ts=1717228812.900000 phase=warmup_start block=8
ts=1717228858.103440 phase=warmup_end block=24
ts=1717228858.103600 phase=drain_start
ts=1717228866.313600 phase=drain_end
ts=1717228866.313700 phase=sleep5_start
ts=1717228871.314930 phase=sleep5_end
ts=1717228871.315000 phase=real_bench_start block=42
ts=1717229058.747100 phase=real_bench_end block=287
ts=1717229058.747200 phase=stop_node_start
ts=1717229063.759203 phase=stop_node_end
ts=1717229063.759300 phase=run_end
```

- [ ] **Step 3: Write `complete-run/aot-node-aot-20260601_120000.log`** (synthetic; mix bench_timing + jit::stats + aot lines)

Use Python to generate this since 245+ block lines is too much to hand-write:

```bash
python3 - <<'PYEOF' > /Users/jiezhang/meili/jie.zhang1_dacs_at_okg.com/114/Documents/repository/xlayer-reth/scripts/tests/fixtures/complete-run/aot-node-aot-20260601_120000.log
import time
from datetime import datetime, timezone
def ts(unix):
    return datetime.fromtimestamp(unix, timezone.utc).strftime('%Y-%m-%dT%H:%M:%S.%fZ')

# AOT store opened (boot)
print(f"{ts(1717228810.700000)}  INFO xlayer::jit::aot: AOT store opened dir=/aot-cache loaded=4 dlopen_us=142000 n_manifests=4")
print(f"{ts(1717228810.750000)}  INFO xlayer::jit: JIT runtime enabled enabled=true blocking=false")

# warmup blocks 8..24 — light traffic (n_total_txs varies 50-200)
import random
random.seed(42)
unix = 1717228812.0
block = 1
for _ in range(7):
    print(f"{ts(unix)}  INFO xlayer::bench_timing: block_phase_breakdown block={block} id=0xaa n_total_txs=0 n_seq_txs=1 n_pool_txs=0 gas_used=21000 pre_exec_us=320 seq_txs_us=1100 pool_exec_us=0 finish_us=5000 total_us=6420")
    unix += 1.0; block += 1
# warmup window 8-24
for b in range(8, 25):
    n_pool = random.randint(80, 220)  # mix of < 100 and >= 100
    n_total = n_pool + 1
    pool_us = n_pool * 380 + random.randint(-2000, 2000)
    gas = n_total * 100000
    print(f"{ts(unix)}  INFO xlayer::bench_timing: block_phase_breakdown block={b} id=0x{b:02x} n_total_txs={n_total} n_seq_txs=1 n_pool_txs={n_pool} gas_used={gas} pre_exec_us=400 seq_txs_us=1200 pool_exec_us={pool_us} finish_us=13000 total_us={400+1200+pool_us+13000}")
    if b % 5 == 0:
        print(f"{ts(unix)}  INFO xlayer::jit::stats: jit_periodic_stats resident=4 compile_ok=4 compile_err=0 dispatched=4 lookup_hits={b*180} lookup_misses=4 pending=0 evictions=0 code_bytes=2147834")
    unix += 1.0

# bench window 42-287 — heavy traffic (n_total_txs 200-490)
for b in range(25, 42):  # gap (drain + sleep)
    print(f"{ts(unix)}  INFO xlayer::bench_timing: block_phase_breakdown block={b} id=0x{b:02x} n_total_txs=2 n_seq_txs=1 n_pool_txs=1 gas_used=42000 pre_exec_us=300 seq_txs_us=1100 pool_exec_us=400 finish_us=5000 total_us=6800")
    if b % 5 == 0:
        print(f"{ts(unix)}  INFO xlayer::jit::stats: jit_periodic_stats resident=4 compile_ok=4 compile_err=0 dispatched=4 lookup_hits={3210+b*5} lookup_misses=18 pending=0 evictions=0 code_bytes=2147834")
    unix += 1.0
for b in range(42, 288):
    n_pool = random.randint(180, 487)
    n_total = n_pool + 1
    pool_us = n_pool * 372 + random.randint(-3000, 3000)
    gas = n_total * 100000
    print(f"{ts(unix)}  INFO xlayer::bench_timing: block_phase_breakdown block={b} id=0x{b:02x} n_total_txs={n_total} n_seq_txs=1 n_pool_txs={n_pool} gas_used={gas} pre_exec_us=350 seq_txs_us=1200 pool_exec_us={pool_us} finish_us=12400 total_us={350+1200+pool_us+12400}")
    if b % 5 == 0:
        hits = 3210 + (b-42)*200
        print(f"{ts(unix)}  INFO xlayer::jit::stats: jit_periodic_stats resident=4 compile_ok=4 compile_err=0 dispatched=4 lookup_hits={hits} lookup_misses=18 pending=0 evictions=0 code_bytes=2147834")
    unix += 0.76  # ~245 blocks in ~187s
PYEOF
```

- [ ] **Step 4: Write `complete-run/aot-result-aot-20260601_120000.out`**

```
============== ERC4337 Test Parameters ==================
Total UOP: 50000
Batch Size: 1
Total Transactions: 50000
Concurrency: 50
Rate Limit: 5000
Call Data Type: APPROVE_ERC20
Starting loadtest ... (Results will be saved to result_20260601_1200.out)
...
loadtest finished tps=287.30 numErrors=0
Final TPS: 287.30
```

- [ ] **Step 5: Write `partial-tslog/aot-cycle-nojit-20260601_120100.tslog`** (missing `run_end`)

```
ts=1717228900.000000 phase=run_start mode=nojit round=1
ts=1717228900.000200 phase=cleanup_start
ts=1717228902.000200 phase=cleanup_end
ts=1717228902.000400 phase=cp_golden_start
ts=1717228910.500000 phase=cp_golden_end
ts=1717228910.500200 phase=node_start
ts=1717228911.500000 phase=rpc_wait_start
ts=1717228912.500000 phase=rpc_ready
ts=1717228912.600000 phase=warmup_start block=8
```
(Truncated — simulates script crashing during warmup.)

- [ ] **Step 6: Write `no-bench-timing/` fixtures** (node log lacks `block_phase_breakdown`)

`no-bench-timing/aot-cycle-nojit-20260601_120200.tslog`: same as complete-run but with `mode=nojit` and timestamps shifted by +200s.

`no-bench-timing/aot-node-nojit-20260601_120200.log`:

```
2026-06-01T12:02:00.000000Z  INFO reth: starting reth node
2026-06-01T12:02:01.000000Z  INFO reth::rpc: RPC server started
2026-06-01T12:02:02.000000Z  INFO xlayer::jit: JIT runtime enabled enabled=false blocking=false
```
(No `block_phase_breakdown` lines — simulates node running off a non-timing branch.)

- [ ] **Step 7: Write `scripts/tests/test_parser.py` skeleton**

```python
"""Unit tests for parse-bench-timing.py.

Run: python3 -m unittest discover -s scripts/tests -v
"""
import unittest
from pathlib import Path
import sys
import importlib.util

# Load the parser module from scripts/parse-bench-timing.py (hyphenated name needs importlib)
HERE = Path(__file__).resolve().parent
PARSER_PATH = HERE.parent / "parse-bench-timing.py"
spec = importlib.util.spec_from_file_location("parse_bench_timing", PARSER_PATH)
ptm = importlib.util.module_from_spec(spec)
sys.modules["parse_bench_timing"] = ptm
spec.loader.exec_module(ptm)

FIXTURES = HERE / "fixtures"


class TestTslogReader(unittest.TestCase):
    def test_complete_tslog_loads(self):
        events = ptm.read_tslog(FIXTURES / "complete-run" / "aot-cycle-aot-20260601_120000.tslog")
        self.assertEqual(events[0]["phase"], "run_start")
        self.assertEqual(events[-1]["phase"], "run_end")
        self.assertEqual(events[0]["mode"], "aot")

    def test_partial_tslog_loads_without_run_end(self):
        events = ptm.read_tslog(FIXTURES / "partial-tslog" / "aot-cycle-nojit-20260601_120100.tslog")
        phases = [e["phase"] for e in events]
        self.assertIn("warmup_start", phases)
        self.assertNotIn("run_end", phases)


if __name__ == "__main__":
    unittest.main()
```

- [ ] **Step 8: Run the test (it should fail because `read_tslog` doesn't exist yet)**

```bash
cd /Users/jiezhang/meili/jie.zhang1_dacs_at_okg.com/114/Documents/repository/xlayer-reth
python3 -m unittest scripts.tests.test_parser -v
```
Expected: `AttributeError: module 'parse_bench_timing' has no attribute 'read_tslog'` — confirming TDD red.

- [ ] **Step 9: Stage fixtures + test scaffold (no commit yet — commit with parser in Task 7)**

```bash
cd /Users/jiezhang/meili/jie.zhang1_dacs_at_okg.com/114/Documents/repository/xlayer-reth
git add scripts/tests/
git status --short scripts/tests/
```
Expected: 7 new files staged (1 init + 1 test + 5 fixture files).

---

### Task 4: Parser — tslog reader (TDD)

Add the `read_tslog(path) -> list[dict]` function to satisfy Task 3 tests.

**Files:**
- Modify: `$XLAYER_RETH/scripts/parse-bench-timing.py`

- [ ] **Step 1: Add `read_tslog` to `scripts/parse-bench-timing.py`**

Insert after the existing imports (around current line 26):

```python
# --- tslog reader ----------------------------------------------------------
# Format: `ts=1717228800.000000 phase=<name> [key=value ...]`
# All keys/values are bareword tokens separated by spaces. Values are kept
# as strings; the caller converts numerics where needed.
TSLOG_KV_RE = re.compile(r"\b([a-z_][a-z0-9_]*)=([^\s]+)")


def read_tslog(path: Path) -> list[dict]:
    """Parse one .tslog file into a list of event dicts.

    Each line yields one dict with at least `ts` (float) and `phase` (str)
    keys, plus any extra `key=value` tokens kept as strings.

    Returns `[]` if the file is missing.
    """
    if not path.exists():
        return []
    events: list[dict] = []
    with path.open(errors="replace") as f:
        for raw in f:
            kv = dict(TSLOG_KV_RE.findall(raw))
            if "ts" not in kv or "phase" not in kv:
                continue
            event = {"phase": kv.pop("phase")}
            try:
                event["ts"] = float(kv.pop("ts"))
            except ValueError:
                continue
            event.update(kv)
            events.append(event)
    return events
```

- [ ] **Step 2: Run tests — should now pass**

```bash
cd /Users/jiezhang/meili/jie.zhang1_dacs_at_okg.com/114/Documents/repository/xlayer-reth
python3 -m unittest scripts.tests.test_parser -v
```
Expected: 2 tests pass, OK.

---

### Task 5: Parser — node log + polycli readers (TDD)

Add three more readers and their tests.

**Files:**
- Modify: `$XLAYER_RETH/scripts/parse-bench-timing.py`
- Modify: `$XLAYER_RETH/scripts/tests/test_parser.py`

- [ ] **Step 1: Add failing tests in `scripts/tests/test_parser.py`**

Append after `TestTslogReader`:

```python
class TestNodeLogReader(unittest.TestCase):
    def test_complete_node_log(self):
        result = ptm.read_node_log(FIXTURES / "complete-run" / "aot-node-aot-20260601_120000.log")
        self.assertGreater(len(result["blocks"]), 250)
        first_real = next(b for b in result["blocks"] if b["block"] == 42)
        self.assertEqual(first_real["n_seq_txs"], 1)
        self.assertGreater(first_real["n_pool_txs"], 100)
        self.assertGreater(len(result["jit_snapshots"]), 10)
        self.assertIsNotNone(result["aot_open"])
        self.assertEqual(result["aot_open"]["loaded"], 4)
        self.assertEqual(result["aot_open"]["dlopen_us"], 142000)

    def test_no_bench_timing_node_log(self):
        result = ptm.read_node_log(FIXTURES / "no-bench-timing" / "aot-node-nojit-20260601_120200.log")
        self.assertEqual(result["blocks"], [])
        self.assertIsNone(result["aot_open"])


class TestPolycliReader(unittest.TestCase):
    def test_polycli_final_tps(self):
        result = ptm.read_polycli_out(FIXTURES / "complete-run" / "aot-result-aot-20260601_120000.out")
        self.assertAlmostEqual(result["final_tps"], 287.30, places=2)
        self.assertEqual(result["num_errors"], 0)
```

- [ ] **Step 2: Run tests — confirm they fail**

```bash
cd /Users/jiezhang/meili/jie.zhang1_dacs_at_okg.com/114/Documents/repository/xlayer-reth
python3 -m unittest scripts.tests.test_parser -v
```
Expected: 2 new tests FAIL with `AttributeError: ... no attribute 'read_node_log'` / `'read_polycli_out'`.

- [ ] **Step 3: Implement `read_node_log` + `read_node_ts` + `read_polycli_out` in parser**

Append to `scripts/parse-bench-timing.py`:

```python
# --- node log reader -------------------------------------------------------
# Tracing emits RFC-3339 timestamps prefix: "2026-06-01T12:02:00.000000Z  INFO ..."
NODE_TS_RE = re.compile(r"^(\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d+Z)")

# Reuse existing KV_RE for `key=number` fields on bench_timing / jit::stats lines.
# For AOT lines we need to grab dlopen_us / loaded / n_manifests too.


def _parse_node_ts(line: str) -> float | None:
    m = NODE_TS_RE.match(line)
    if not m:
        return None
    from datetime import datetime
    try:
        # `fromisoformat` accepts "...Z" only on Python 3.11+; strip the trailing Z to be safe.
        return datetime.fromisoformat(m.group(1).replace("Z", "+00:00")).timestamp()
    except ValueError:
        return None


def read_node_log(path: Path) -> dict:
    """Parse a node log into structured pieces.

    Returns: {"blocks": [...], "jit_snapshots": [...], "aot_open": {...}|None}
      - blocks: each dict has {block, n_total_txs, n_seq_txs, n_pool_txs, gas_used,
                               pre_exec_us, seq_txs_us, pool_exec_us, finish_us,
                               total_us, ts (float unix)}
      - jit_snapshots: each dict has {ts, resident, compile_ok, lookup_hits, ...}
      - aot_open: {dlopen_us, loaded, n_manifests, ts} or None
    """
    blocks: list[dict] = []
    jit_snapshots: list[dict] = []
    aot_open: dict | None = None
    if not path.exists():
        return {"blocks": blocks, "jit_snapshots": jit_snapshots, "aot_open": aot_open}

    with path.open(errors="replace") as f:
        for raw in f:
            line = ANSI_RE.sub("", raw)
            ts = _parse_node_ts(line)
            if "block_phase_breakdown" in line:
                kv = {k: int(v) for k, v in KV_RE.findall(line)}
                if "block" in kv:
                    kv["ts"] = ts
                    blocks.append(kv)
            elif "jit_periodic_stats" in line:
                kv = {k: int(v) for k, v in KV_RE.findall(line)}
                kv["ts"] = ts
                jit_snapshots.append(kv)
            elif "AOT store opened" in line:
                kv = {k: int(v) for k, v in KV_RE.findall(line)}
                kv["ts"] = ts
                aot_open = kv
    return {"blocks": blocks, "jit_snapshots": jit_snapshots, "aot_open": aot_open}


# --- polycli out reader ----------------------------------------------------
POLYCLI_TPS_RE = re.compile(r"Final TPS:\s*([\d.]+)")
POLYCLI_ERR_RE = re.compile(r"numErrors=(\d+)")


def read_polycli_out(path: Path) -> dict:
    """Parse polycli output for Final TPS + numErrors. Missing → None."""
    if not path.exists():
        return {"final_tps": None, "num_errors": None}
    text = path.read_text(errors="replace")
    tps_m = POLYCLI_TPS_RE.search(text)
    err_m = POLYCLI_ERR_RE.search(text)
    return {
        "final_tps": float(tps_m.group(1)) if tps_m else None,
        "num_errors": int(err_m.group(1)) if err_m else None,
    }
```

- [ ] **Step 4: Run tests — should pass**

```bash
cd /Users/jiezhang/meili/jie.zhang1_dacs_at_okg.com/114/Documents/repository/xlayer-reth
python3 -m unittest scripts.tests.test_parser -v
```
Expected: 4 tests pass, OK.

---

### Task 6: Parser — merge + window slicing + JSON output (TDD)

Build the `process_run(run_dir, mode, ts)` entry point that ties everything into the JSON schema from spec §6.

**Files:**
- Modify: `$XLAYER_RETH/scripts/parse-bench-timing.py`
- Modify: `$XLAYER_RETH/scripts/tests/test_parser.py`

- [ ] **Step 1: Add failing tests**

Append to `test_parser.py`:

```python
class TestProcessRun(unittest.TestCase):
    def test_complete_run_json(self):
        result = ptm.process_run(
            run_dir=FIXTURES / "complete-run",
            mode="aot",
            timestamp="20260601_120000",
        )
        self.assertEqual(result["schema_version"], 1)
        self.assertEqual(result["meta"]["mode"], "aot")

        # wall_clock_by_phase: 10 fields, all populated
        wc = result["wall_clock_by_phase"]
        for field in ("cleanup_us", "cp_golden_us", "node_start_us",
                      "rpc_wait_us", "warmup_polycli_us", "drain_txpool_us",
                      "sleep5_us", "real_bench_us", "stop_node_us", "total_run_us"):
            self.assertIn(field, wc)
            self.assertIsNotNone(wc[field])
            self.assertGreater(wc[field], 0)

        # bench_window
        bw = result["bench_window"]
        self.assertEqual(bw["warmup_start_block"], 8)
        self.assertEqual(bw["warmup_end_block"], 24)
        self.assertEqual(bw["real_start_block"], 42)
        self.assertEqual(bw["real_end_block"], 287)

        # chain_blocks_real: filter heavy blocks (>= 100 txs)
        cbr = result["chain_blocks_real"]
        self.assertGreater(cbr["n_blocks"], 100)
        self.assertGreater(cbr["phase_us"]["pool_exec"]["p50"], 0)

        # jit_stats_delta
        jsd = result["jit_stats_delta"]
        self.assertGreater(jsd["lookup_hits_delta"], 0)

        # aot
        self.assertIsNotNone(result["aot"])
        self.assertEqual(result["aot"]["dlopen_us"], 142000)

        # tps
        self.assertAlmostEqual(result["tps"]["polycli_final"], 287.30, places=2)
        self.assertGreater(result["tps"]["effective_chain"], 0)
        self.assertGreater(result["tps"]["wall_clock_bench_ratio"], 0)
        self.assertLess(result["tps"]["wall_clock_bench_ratio"], 1.0)

    def test_partial_tslog_does_not_crash(self):
        result = ptm.process_run(
            run_dir=FIXTURES / "partial-tslog",
            mode="nojit",
            timestamp="20260601_120100",
        )
        self.assertIn("tslog_partial", " ".join(result["meta"]["warnings"]))
        self.assertIsNone(result["wall_clock_by_phase"].get("total_run_us"))

    def test_no_bench_timing_emits_warning(self):
        result = ptm.process_run(
            run_dir=FIXTURES / "no-bench-timing",
            mode="nojit",
            timestamp="20260601_120200",
        )
        self.assertIsNone(result["chain_blocks_real"])
        self.assertIn("no_bench_timing_lines", " ".join(result["meta"]["warnings"]))
```

- [ ] **Step 2: Run tests — confirm they fail**

```bash
cd /Users/jiezhang/meili/jie.zhang1_dacs_at_okg.com/114/Documents/repository/xlayer-reth
python3 -m unittest scripts.tests.test_parser -v
```
Expected: 3 new tests FAIL with `AttributeError: ... 'process_run'`.

- [ ] **Step 3: Implement `process_run` + helpers**

Append to `scripts/parse-bench-timing.py`:

```python
# --- merge + window slicing ------------------------------------------------
PHASE_PAIRS = [
    ("cleanup", "cleanup_start", "cleanup_end"),
    ("cp_golden", "cp_golden_start", "cp_golden_end"),
    ("node_start", "node_start", "rpc_wait_start"),  # node_start has no _end; ends when rpc_wait starts
    ("rpc_wait", "rpc_wait_start", "rpc_ready"),
    ("warmup_polycli", "warmup_start", "warmup_end"),
    ("drain_txpool", "drain_start", "drain_end"),
    ("sleep5", "sleep5_start", "sleep5_end"),
    ("real_bench", "real_bench_start", "real_bench_end"),
    ("stop_node", "stop_node_start", "stop_node_end"),
]


def _phase_durations(events: list[dict]) -> dict:
    """Return {<name>_us: int} for each phase pair found. Missing phases → key absent."""
    by_phase = {e["phase"]: e for e in events}
    out = {}
    for name, start_key, end_key in PHASE_PAIRS:
        s = by_phase.get(start_key)
        e = by_phase.get(end_key)
        if s and e:
            out[f"{name}_us"] = int((e["ts"] - s["ts"]) * 1_000_000)
    # total_run_us: from run_start to run_end
    if "run_start" in by_phase and "run_end" in by_phase:
        out["total_run_us"] = int((by_phase["run_end"]["ts"] - by_phase["run_start"]["ts"]) * 1_000_000)
    return out


def _bench_window(events: list[dict]) -> dict:
    """Pull the 4 block sentinels from tslog events."""
    by_phase = {e["phase"]: e for e in events}
    out = {}
    for shell_phase, json_key in [
        ("warmup_start", "warmup_start_block"),
        ("warmup_end", "warmup_end_block"),
        ("real_bench_start", "real_start_block"),
        ("real_bench_end", "real_end_block"),
    ]:
        evt = by_phase.get(shell_phase)
        if evt and "block" in evt and evt["block"] != "unknown":
            try:
                out[json_key] = int(evt["block"])
            except ValueError:
                out[json_key] = None
        else:
            out[json_key] = None
    return out


def _percentiles(values: list[int]) -> dict:
    if not values:
        return {"p50": None, "p95": None, "mean": None}
    s = sorted(values)
    n = len(s)
    return {
        "p50": s[n // 2],
        "p95": s[min(n - 1, int(n * 0.95))],
        "mean": sum(s) // n,
    }


def _chain_blocks_real(node_blocks: list[dict], window: dict, min_block_txs: int = 100) -> dict | None:
    """Filter to [real_start_block, real_end_block] then drop n_total_txs < min_block_txs."""
    if not node_blocks:
        return None
    rs = window.get("real_start_block")
    re_ = window.get("real_end_block")
    if rs is None or re_ is None:
        return None
    in_window = [b for b in node_blocks if rs <= b.get("block", -1) <= re_]
    heavy = [b for b in in_window if b.get("n_total_txs", 0) >= min_block_txs]
    if not heavy:
        return None
    phase_fields = ("pre_exec_us", "seq_txs_us", "pool_exec_us", "finish_us", "total_us")
    phase_us = {
        f.replace("_us", ""): _percentiles([b[f] for b in heavy if f in b]) for f in phase_fields
    }
    tx_fields = ("n_seq_txs", "n_pool_txs", "n_total_txs")
    tx_counts = {}
    for f in tx_fields:
        v = [b[f] for b in heavy if f in b]
        tx_counts[f] = {"avg": sum(v) / len(v) if v else None, "max": max(v) if v else None}
    sum_pool_us = sum(b.get("pool_exec_us", 0) for b in heavy)
    sum_pool_tx = sum(b.get("n_pool_txs", 0) for b in heavy)
    sum_total_us = sum(b.get("total_us", 0) for b in heavy)
    sum_gas = sum(b.get("gas_used", 0) for b in heavy)
    per_tx_pool_us = sum_pool_us // sum_pool_tx if sum_pool_tx else None
    per_mgas_total_us = int(sum_total_us / (sum_gas / 1_000_000)) if sum_gas else None
    return {
        "n_blocks": len(heavy),
        "phase_us": phase_us,
        "tx_counts": tx_counts,
        "per_tx_pool_us": per_tx_pool_us,
        "per_mgas_total_us": per_mgas_total_us,
    }


def _jit_stats_delta(jit_snapshots: list[dict], events: list[dict]) -> dict:
    """Compute jit counter delta between bench window start/end snapshots."""
    by_phase = {e["phase"]: e for e in events}
    real_start = by_phase.get("real_bench_start")
    real_end = by_phase.get("real_bench_end")
    if not (real_start and real_end and jit_snapshots):
        return {}
    # First snapshot ts >= real_start.ts
    s = next((sn for sn in jit_snapshots if sn["ts"] and sn["ts"] >= real_start["ts"]), None)
    # Last snapshot ts <= real_end.ts
    e = next((sn for sn in reversed(jit_snapshots) if sn["ts"] and sn["ts"] <= real_end["ts"]), None)
    if not (s and e):
        return {}
    delta = {
        "real_window_seconds": int(real_end["ts"] - real_start["ts"]),
        "compile_ok_delta": e["compile_ok"] - s["compile_ok"],
        "compile_ok_total": e["compile_ok"],
        "lookup_hits_delta": e["lookup_hits"] - s["lookup_hits"],
        "lookup_misses_delta": e["lookup_misses"] - s["lookup_misses"],
        "evictions_delta": e["evictions"] - s["evictions"],
        "code_bytes_final": e.get("code_bytes", 0),
    }
    total = delta["lookup_hits_delta"] + delta["lookup_misses_delta"]
    delta["lookup_hit_rate"] = (delta["lookup_hits_delta"] / total) if total else None
    # Warmup window (warmup_start → warmup_end) — same logic
    warmup_start = by_phase.get("warmup_start")
    warmup_end = by_phase.get("warmup_end")
    if warmup_start and warmup_end:
        ws = next((sn for sn in jit_snapshots if sn["ts"] and sn["ts"] >= warmup_start["ts"]), None)
        we = next((sn for sn in reversed(jit_snapshots) if sn["ts"] and sn["ts"] <= warmup_end["ts"]), None)
        if ws and we:
            delta["compile_ok_warmup"] = we["compile_ok"] - ws["compile_ok"]
            delta["lookup_hits_warmup"] = we["lookup_hits"] - ws["lookup_hits"]
    return delta


def process_run(run_dir: Path, mode: str, timestamp: str) -> dict:
    """Build the JSON schema for one run. Returns the schema dict (caller writes JSON)."""
    tslog = run_dir / f"aot-cycle-{mode}-{timestamp}.tslog"
    node = run_dir / f"aot-node-{mode}-{timestamp}.log"
    polycli = run_dir / f"aot-result-{mode}-{timestamp}.out"

    warnings: list[str] = []
    events = read_tslog(tslog)
    if not events:
        warnings.append("tslog_missing")
    elif not any(e["phase"] == "run_end" for e in events):
        warnings.append("tslog_partial: run_end missing")

    node_data = read_node_log(node)
    if not node_data["blocks"]:
        warnings.append("no_bench_timing_lines")

    polycli_data = read_polycli_out(polycli)

    wall_clock = _phase_durations(events)
    window = _bench_window(events)
    chain_blocks_real = _chain_blocks_real(node_data["blocks"], window)
    jit_delta = _jit_stats_delta(node_data["jit_snapshots"], events)

    # tps.wall_clock_bench_ratio = sum(chain block total_us in real window) / real_bench_us
    if chain_blocks_real and "real_bench_us" in wall_clock:
        rs = window.get("real_start_block")
        re_ = window.get("real_end_block")
        sum_total_us = sum(b.get("total_us", 0) for b in node_data["blocks"] if rs <= b.get("block", -1) <= re_)
        ratio = sum_total_us / wall_clock["real_bench_us"] if wall_clock["real_bench_us"] else None
    else:
        sum_total_us = 0
        ratio = None
    effective_chain = None
    if chain_blocks_real and chain_blocks_real["tx_counts"]["n_total_txs"]["avg"]:
        total_tx = chain_blocks_real["tx_counts"]["n_total_txs"]["avg"] * chain_blocks_real["n_blocks"]
        if sum_total_us:
            effective_chain = total_tx / (sum_total_us / 1_000_000)

    # Normalize AOT field names to match the JSON schema in the design spec:
    # the Rust log emits `loaded` (legacy) which the schema calls `n_dylib_loaded`.
    aot_out = None
    if node_data["aot_open"]:
        src = node_data["aot_open"]
        aot_out = {
            "dlopen_us": src.get("dlopen_us"),
            "n_dylib_loaded": src.get("loaded"),
            "n_manifests": src.get("n_manifests", src.get("loaded")),
        }

    return {
        "schema_version": 1,
        "meta": {
            "mode": mode,
            "round": None,
            "timestamp": timestamp,
            "warnings": warnings,
        },
        "wall_clock_by_phase": wall_clock if wall_clock else {"total_run_us": None},
        "bench_window": window,
        "chain_blocks_real": chain_blocks_real,
        "jit_stats_delta": jit_delta,
        "aot": aot_out,
        "tps": {
            "polycli_final": polycli_data["final_tps"],
            "polycli_errors": polycli_data["num_errors"],
            "effective_chain": effective_chain,
            "per_tx_pool_us": chain_blocks_real["per_tx_pool_us"] if chain_blocks_real else None,
            "wall_clock_bench_ratio": ratio,
        },
    }
```

- [ ] **Step 4: Run tests — should all pass**

```bash
cd /Users/jiezhang/meili/jie.zhang1_dacs_at_okg.com/114/Documents/repository/xlayer-reth
python3 -m unittest scripts.tests.test_parser -v
```
Expected: 7 tests pass, OK.

---

### Task 7: Parser — CLI + terminal tables + commit

Wire `process_run` into the CLI; keep legacy `--label` mode working; emit JSON to disk + 4 terminal tables.

**Files:**
- Modify: `$XLAYER_RETH/scripts/parse-bench-timing.py`

- [ ] **Step 1: Replace `main()` in `parse-bench-timing.py`**

Replace the existing `main()` (and remove old `summarize`/`cmp_table` if they conflict — keep them for `--label` mode):

```python
def render_tables(data: dict) -> str:
    """Render 4 terminal tables from a process_run() result."""
    out = []
    meta = data["meta"]
    out.append(f"=== {meta['mode']} @ {meta['timestamp']} ===")
    if meta["warnings"]:
        out.append("WARNINGS: " + "; ".join(meta["warnings"]))

    # Table 1: wall-clock by phase
    wc = data.get("wall_clock_by_phase") or {}
    out.append("\n--- wall-clock by phase ---")
    out.append(f"{'phase':<20} {'us':>14} {'fraction':>10}")
    total = wc.get("total_run_us") or 0
    for k, v in wc.items():
        if v is None:
            out.append(f"{k:<20} {'n/a':>14} {'n/a':>10}")
            continue
        frac = (v / total * 100) if total and k != "total_run_us" else 100.0 if k == "total_run_us" else 0
        out.append(f"{k:<20} {v:>14} {frac:>9.1f}%")

    # Table 2: chain block phases
    cbr = data.get("chain_blocks_real")
    out.append(f"\n--- chain block phases (real window, n={cbr['n_blocks'] if cbr else 0}, filtered heavy) ---")
    if cbr:
        out.append(f"{'phase':<14} {'p50':>10} {'p95':>10} {'mean':>10}")
        for k, v in cbr["phase_us"].items():
            out.append(f"{k:<14} {str(v.get('p50','n/a')):>10} {str(v.get('p95','n/a')):>10} {str(v.get('mean','n/a')):>10}")
        out.append(f"per_tx_pool_us = {cbr['per_tx_pool_us']}   per_mgas_total_us = {cbr['per_mgas_total_us']}")
    else:
        out.append("  (no bench_timing data in window)")

    # Table 3: JIT stats delta
    jsd = data.get("jit_stats_delta") or {}
    out.append("\n--- JIT stats delta (real window) ---")
    if jsd:
        for k in ("real_window_seconds", "compile_ok_delta", "lookup_hits_delta",
                  "lookup_misses_delta", "lookup_hit_rate", "evictions_delta"):
            if k in jsd:
                v = jsd[k]
                if isinstance(v, float):
                    out.append(f"  {k:<22} {v:.5f}")
                else:
                    out.append(f"  {k:<22} {v}")
    else:
        out.append("  (no jit::stats snapshots in window)")

    # Table 4: TPS
    tps = data["tps"]
    out.append("\n--- TPS + ratios ---")
    for k, v in tps.items():
        if v is None:
            out.append(f"  {k:<26} n/a")
        elif isinstance(v, float):
            out.append(f"  {k:<26} {v:.4f}")
        else:
            out.append(f"  {k:<26} {v}")
    return "\n".join(out)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--run-dir", type=Path, help="bench-results directory containing aot-cycle/aot-node/aot-result files")
    ap.add_argument("--mode", choices=["nojit", "jit_cold", "aot"], help="run mode (required with --run-dir)")
    ap.add_argument("--timestamp", help="run timestamp YYYYMMDD_HHMMSS (required with --run-dir)")
    ap.add_argument("--json-out", type=Path, help="explicit JSON output path (default: <run-dir>/cycle-timing-<mode>-<ts>.json)")
    ap.add_argument("--min-block-txs", type=int, default=100, help="filter blocks with n_total_txs below this (default: 100)")
    # Legacy mode (kept for backwards compatibility):
    ap.add_argument("--label", action="append", default=[], help="legacy: group label for files")
    ap.add_argument("files", nargs="*", help="legacy: log files for --label mode")
    args = ap.parse_args()

    if args.run_dir:
        if not (args.mode and args.timestamp):
            ap.error("--mode and --timestamp are required with --run-dir")
        data = process_run(args.run_dir, args.mode, args.timestamp)
        json_path = args.json_out or args.run_dir / f"cycle-timing-{args.mode}-{args.timestamp}.json"
        import json
        json_path.write_text(json.dumps(data, indent=2))
        print(f"JSON written: {json_path}")
        print(render_tables(data))
        return

    # Legacy --label mode (unchanged from prior version)
    _legacy_main(args)


def _legacy_main(args):
    """Run the original --label X.log --label Y.log behavior (preserved verbatim from prior main)."""
    if not args.label and not args.files:
        # Print help if no usable flags
        import sys
        print("usage: parse-bench-timing.py --run-dir DIR --mode MODE --timestamp TS")
        print("   or: parse-bench-timing.py --label nojit X.log --label aot Y.log")
        sys.exit(1)
    # Re-parse argv to interleave labels and files (existing logic)
    import sys as _sys
    groups: list[tuple[str, list[Path]]] = []
    current_label = None
    current_files: list[Path] = []
    raw = _sys.argv[1:]
    i = 0
    while i < len(raw):
        a = raw[i]
        if a == "--label":
            if current_label is not None:
                groups.append((current_label, current_files))
            current_label = raw[i + 1]
            current_files = []
            i += 2
        elif a in ("--run-dir", "--mode", "--timestamp", "--json-out", "--min-block-txs"):
            i += 2  # skip flag + value
        else:
            current_files.append(Path(a))
            i += 1
    if current_label is not None:
        groups.append((current_label, current_files))

    parsed_groups: list[tuple[str, dict]] = []
    for label, files in groups:
        parsed = merge([parse_log(p) for p in files])
        parsed_groups.append((label, parsed))
        summarize(label, parsed)
    cmp_table(parsed_groups)
```

(Keep the existing `parse_log`, `merge`, `summarize`, `cmp_table` functions intact for the legacy path.)

- [ ] **Step 2: Smoke-test legacy mode still works**

```bash
cd /Users/jiezhang/meili/jie.zhang1_dacs_at_okg.com/114/Documents/repository/xlayer-reth
ls bench-results/aot-node-nojit-*.log | head -1 | xargs -I{} python3 scripts/parse-bench-timing.py --label nojit {}
```
Expected: prints `=== nojit ===` block with phase percentiles (works on any historical log).

- [ ] **Step 3: Smoke-test new mode on fixtures**

```bash
cd /Users/jiezhang/meili/jie.zhang1_dacs_at_okg.com/114/Documents/repository/xlayer-reth
python3 scripts/parse-bench-timing.py \
    --run-dir scripts/tests/fixtures/complete-run \
    --mode aot --timestamp 20260601_120000 \
    --json-out /tmp/test-out.json
cat /tmp/test-out.json | python3 -c "import json,sys; d=json.load(sys.stdin); print('OK schema=' + str(d['schema_version']) + ' wall_clock keys=' + str(len(d['wall_clock_by_phase'])))"
```
Expected: prints JSON path + 4 tables; cat output: `OK schema=1 wall_clock keys=10`.

- [ ] **Step 4: Run full unit-test suite**

```bash
cd /Users/jiezhang/meili/jie.zhang1_dacs_at_okg.com/114/Documents/repository/xlayer-reth
python3 -m unittest scripts.tests.test_parser -v
```
Expected: 7 tests pass.

- [ ] **Step 5: Commit parser + tests + fixtures together**

```bash
cd /Users/jiezhang/meili/jie.zhang1_dacs_at_okg.com/114/Documents/repository/xlayer-reth
git add scripts/parse-bench-timing.py scripts/tests/
git commit -m "$(cat <<'EOF'
feat(bench): rewrite parse-bench-timing.py to emit per-run JSON + cycle table

Adds --run-dir / --mode / --timestamp CLI for the new per-run workflow:
reads .tslog (shell phase boundaries), node log (block_phase_breakdown,
jit::stats, AOT store opened), and polycli .out (Final TPS), merges via
block-number bench window, writes cycle-timing-<MODE>-<TS>.json plus 4
terminal tables.

Default filter: n_total_txs >= 100 (drops dev block-time heartbeat).
JIT stats delta computed for both warmup and real windows separately.
Legacy --label mode preserved for historical log triage.

Includes unittest-based fixture tests (no pytest dep): 7 tests covering
complete-run, partial-tslog, and no-bench-timing scenarios.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Phase C — Rust additions

### Task 8: op-reth — emit `n_seq_txs` and `n_pool_txs`

Add 2 fields to the existing `block_phase_breakdown` info!.

**Files:**
- Modify: `$OP_RETH/crates/payload/src/builder.rs` (around lines 380-440)

- [ ] **Step 1: Read the current builder.rs phase block to confirm line numbers**

```bash
cd /Users/jiezhang/meili/jie.zhang1_dacs_at_okg.com/114/Documents/repository/xlayer-reth/deps/optimism/rust/op-reth
grep -n "execute_sequencer_transactions\|cumulative_tx_count\|block_phase_breakdown" crates/payload/src/builder.rs
```
Expected: shows ~5 lines including `execute_sequencer_transactions`, `cumulative_gas_used`, and `block_phase_breakdown` (the info! call).

- [ ] **Step 2: Add `n_seq_txs` snapshot and `n_pool_txs` compute**

Edit `crates/payload/src/builder.rs`. Find this block:

```rust
        // 2. execute sequencer transactions
        let mut info = ctx.execute_sequencer_transactions(&mut builder)?;
        let t2 = Instant::now();
```

Replace with:

```rust
        // 2. execute sequencer transactions
        let mut info = ctx.execute_sequencer_transactions(&mut builder)?;
        let n_seq_txs = info.cumulative_tx_count;
        let t2 = Instant::now();
```

Then find:

```rust
        let t3 = Instant::now();
        let cumulative_gas_used = info.cumulative_gas_used;
```

Replace with:

```rust
        let t3 = Instant::now();
        let cumulative_gas_used = info.cumulative_gas_used;
        let n_pool_txs = info.cumulative_tx_count - n_seq_txs;
```

Then find the `info!` call and add the two fields:

```rust
        info!(
            target: "xlayer::bench_timing",
            block = block_number_for_log,
            id = %payload_id_for_log,
            n_total_txs = n_total_txs,
            n_seq_txs = n_seq_txs,
            n_pool_txs = n_pool_txs,
            gas_used = cumulative_gas_used,
            pre_exec_us = (t1 - t0).as_micros() as u64,
            seq_txs_us = (t2 - t1).as_micros() as u64,
            pool_exec_us = (t3 - t2).as_micros() as u64,
            finish_us = (t4 - t3).as_micros() as u64,
            total_us = (t4 - t0).as_micros() as u64,
            "block_phase_breakdown"
        );
```

**Note:** Check the actual type of `info.cumulative_tx_count` — if it's not `u64`, the subtraction may need a cast. The existing field types in `ExecutionInfo` are likely `usize` or `u64`; verify the type matches what `info!` expects. If a type error appears, add `as u64` cast on both `n_seq_txs` and `n_pool_txs`.

- [ ] **Step 3: Build op-reth to verify**

```bash
cd /Users/jiezhang/meili/jie.zhang1_dacs_at_okg.com/114/Documents/repository/xlayer-reth
# Build the relevant package only — full xlayer-reth build takes long with LLVM
cargo check -p reth-optimism-payload-builder --release 2>&1 | tail -20
```
Expected: `Finished` (no errors).

If you get a type error on `info.cumulative_tx_count`, add `as u64` and rebuild.

- [ ] **Step 4: Commit the op-reth change**

```bash
cd /Users/jiezhang/meili/jie.zhang1_dacs_at_okg.com/114/Documents/repository/xlayer-reth/deps/optimism/rust/op-reth
git status --short crates/payload/src/builder.rs
git add crates/payload/src/builder.rs
git commit -m "$(cat <<'EOF'
feat(bench): emit n_seq_txs and n_pool_txs in block_phase_breakdown

Splits the per-block tx count into sequencer (L1 deposit) and pool
(mempool) buckets so per-tx attribution in pool_exec_us isn't muddied
by the 1-2 deposit txs. Conservation: n_seq + n_pool == n_total.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
git log --oneline -3
```
Expected: 2 commits on top of base.

---

### Task 9: xlayer-reth — bump op-reth submodule pointer

The submodule HEAD moved (Task 8). Update the pointer in xlayer-reth.

**Files:**
- Modify: `$XLAYER_RETH/deps/optimism` (submodule pointer)

- [ ] **Step 1: Verify submodule status**

```bash
cd /Users/jiezhang/meili/jie.zhang1_dacs_at_okg.com/114/Documents/repository/xlayer-reth
git submodule status deps/optimism
git status --short deps/optimism
```
Expected: `M deps/optimism` indicating the pointer has moved.

- [ ] **Step 2: Stage and commit**

```bash
cd /Users/jiezhang/meili/jie.zhang1_dacs_at_okg.com/114/Documents/repository/xlayer-reth
git add deps/optimism
git commit -m "$(cat <<'EOF'
chore(bench): bump op-reth submodule to xl/jit-bench-timing HEAD

Picks up:
- feat(bench): emit per-block phase breakdown for OpBuilder::build
- feat(bench): emit n_seq_txs and n_pool_txs in block_phase_breakdown

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
git log --oneline -3
```

---

### Task 10: xlayer-reth — emit `dlopen_us` on AOT store open

Add `Instant::now()` around the `PersistentArtifactStore::open` call and emit `dlopen_us` in the existing `xlayer::jit::aot` info!.

**Files:**
- Modify: `$XLAYER_RETH/crates/builder/src/evm_jit/executor.rs` (around lines 59-83)

- [ ] **Step 1: Check the existing AOT open call site**

```bash
cd /Users/jiezhang/meili/jie.zhang1_dacs_at_okg.com/114/Documents/repository/xlayer-reth
grep -n "AOT store opened\|PersistentArtifactStore::open" crates/builder/src/evm_jit/executor.rs
```
Expected: shows the `tracing::info!` for `"AOT store opened"` around line 65 and the `PersistentArtifactStore::open(path.clone())` call around line 62.

- [ ] **Step 2: Wrap with `Instant::now()` and add fields**

Edit `crates/builder/src/evm_jit/executor.rs`. Find:

```rust
fn build_aot_store_from_env() -> Option<Arc<dyn ArtifactStore>> {
    let dir = std::env::var("XLAYER_AOT_DIR").ok().filter(|d| !d.is_empty())?;
    let path = expand_tilde(&dir);
    match PersistentArtifactStore::open(path.clone()) {
        Ok(store) => {
            let loaded = store.len();
            tracing::info!(
                target: "xlayer::jit::aot",
                dir = %path.display(),
                loaded,
                "AOT store opened"
            );
            Some(Arc::new(store) as Arc<dyn ArtifactStore>)
        }
```

Replace with:

```rust
fn build_aot_store_from_env() -> Option<Arc<dyn ArtifactStore>> {
    let dir = std::env::var("XLAYER_AOT_DIR").ok().filter(|d| !d.is_empty())?;
    let path = expand_tilde(&dir);
    let t_open = std::time::Instant::now();
    match PersistentArtifactStore::open(path.clone()) {
        Ok(store) => {
            let dlopen_us = t_open.elapsed().as_micros() as u64;
            let loaded = store.len();
            tracing::info!(
                target: "xlayer::jit::aot",
                dir = %path.display(),
                loaded,
                dlopen_us,
                n_manifests = loaded,
                "AOT store opened"
            );
            Some(Arc::new(store) as Arc<dyn ArtifactStore>)
        }
```

(`n_manifests` equals `loaded` here because only manifest-paired dylibs are counted; if the implementation diverges in future this gives a separate hook.)

- [ ] **Step 3: Build to verify**

```bash
cd /Users/jiezhang/meili/jie.zhang1_dacs_at_okg.com/114/Documents/repository/xlayer-reth
cargo check -p xlayer-builder --release 2>&1 | tail -10
```
Expected: `Finished`.

- [ ] **Step 4: Commit**

```bash
cd /Users/jiezhang/meili/jie.zhang1_dacs_at_okg.com/114/Documents/repository/xlayer-reth
git add crates/builder/src/evm_jit/executor.rs
git commit -m "$(cat <<'EOF'
feat(bench): emit dlopen_us on AOT store opened

Times the PersistentArtifactStore::open() call (dylib scan + manifest
verify) and emits dlopen_us alongside the existing loaded count. Makes
AOT mode's "free at startup" cost explicit and comparable to JIT cold
compile time.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Phase D — Shell additions

### Task 11: `run-bench-aot.sh` — add `ts_log` helper + phase boundaries

Instrument 9 phase boundaries in the existing script.

**Files:**
- Modify: `$XLAYER_RETH/run-bench-aot.sh`

- [ ] **Step 1: Add the `ts_log` helper + TSLOG path declaration at the top of the script**

Insert after line 34 (the existing `WARMUP_LOG="..."` line) and before "# Helper functions":

```bash
TSLOG="$RESULT_DIR/aot-cycle-${MODE}-${TIMESTAMP}.tslog"
ts_log() {
    # phase=$1 [k=v ...]
    local ts
    ts=$(python3 -c 'import time; print(f"{time.time():.6f}")')
    echo "ts=$ts phase=$1 ${*:2}" >> "$TSLOG"
}
```

- [ ] **Step 2: Instrument boundaries inside `start_node`**

In `start_node()` (around line 40), add `ts_log` markers. Replace:

```bash
start_node() {
    local jit_flag="$1"
    local aot_dir="$2"
    local logfile="$3"
    pkill -9 -f xlayer-reth-jit 2>/dev/null || true
    sleep 2
    rm -rf "$PROJECT_ROOT/jit-data" "$PROJECT_ROOT/jit-logs"
    rm -f "$PROJECT_ROOT/reth.ipc"
    # If a golden state snapshot exists, ...
```

with:

```bash
start_node() {
    local jit_flag="$1"
    local aot_dir="$2"
    local logfile="$3"
    ts_log cleanup_start
    pkill -9 -f xlayer-reth-jit 2>/dev/null || true
    sleep 2
    rm -rf "$PROJECT_ROOT/jit-data" "$PROJECT_ROOT/jit-logs"
    rm -f "$PROJECT_ROOT/reth.ipc"
    ts_log cleanup_end
    # If a golden state snapshot exists, ...
```

Find the `cp -R "$PROJECT_ROOT/golden-state" ...` line inside `start_node` and bracket it:

```bash
    if [ -d "$PROJECT_ROOT/golden-state" ]; then
        echo "[start_node] restoring golden-state → jit-data"
        ts_log cp_golden_start
        cp -R "$PROJECT_ROOT/golden-state" "$PROJECT_ROOT/jit-data"
        ts_log cp_golden_end
    fi
```

Just before the `./run.sh > "$logfile" 2>&1 &` line, add `ts_log node_start`.

- [ ] **Step 3: Instrument `wait_for_rpc`**

Replace the function with:

```bash
wait_for_rpc() {
    ts_log rpc_wait_start
    for i in $(seq 1 60); do
        if curl -s http://127.0.0.1:8545 -X POST \
             -H 'Content-Type: application/json' \
             -d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}' 2>/dev/null \
           | grep -q result; then
            echo "RPC ready (${i}s)"
            ts_log rpc_ready
            return 0
        fi
        sleep 1
    done
    echo "ERROR: RPC not ready in 60s" >&2
    return 1
}
```

- [ ] **Step 4: Instrument the main bench section (Step B)**

In the section starting `# Step B: real measurement` (around line 161), add:

```bash
ts_log run_start mode=$MODE
NODE_PID=$(start_node $JIT_FLAG "$AOT_DIR_ARG" "$NODE_LOG")
echo "node PID=$NODE_PID, log=$NODE_LOG"
wait_for_rpc
```

Around the warmup call:

```bash
ts_log warmup_start
warmup_polycli "$WARMUP_LOG"
ts_log warmup_end
```

Around drain:

```bash
ts_log drain_start
drain_txpool
ts_log drain_end

ts_log sleep5_start
sleep 5
ts_log sleep5_end
```

Around the real bench:

```bash
ts_log real_bench_start
./2-bench.sh 2>&1 | tee "$RESULT_OUT"
BENCH_EXIT=${PIPESTATUS[0]}
ts_log real_bench_end
```

Around stop_node:

```bash
ts_log stop_node_start
stop_node "$NODE_PID"
ts_log stop_node_end
ts_log run_end
```

- [ ] **Step 5: Smoke-test with the fastest mode (nojit)**

```bash
cd /Users/jiezhang/meili/jie.zhang1_dacs_at_okg.com/114/Documents/repository/xlayer-reth
# Sanity: ensure binary + golden state exist
ls -la bin/xlayer-reth-jit golden-state/ aot-cache/ 2>&1 | head
MODE=nojit ./run-bench-aot.sh 2>&1 | tail -20
```
Expected: bench completes; `bench-results/aot-cycle-nojit-*.tslog` exists.

- [ ] **Step 6: Inspect the tslog**

```bash
cd /Users/jiezhang/meili/jie.zhang1_dacs_at_okg.com/114/Documents/repository/xlayer-reth
ls -lt bench-results/aot-cycle-nojit-*.tslog | head -1
cat $(ls -t bench-results/aot-cycle-nojit-*.tslog | head -1)
```
Expected: ~16 lines, monotonic timestamps, every `_start` paired with `_end`.

- [ ] **Step 7: Commit**

```bash
cd /Users/jiezhang/meili/jie.zhang1_dacs_at_okg.com/114/Documents/repository/xlayer-reth
git add run-bench-aot.sh
git commit -m "$(cat <<'EOF'
feat(bench): add ts_log helper + phase boundaries to run-bench-aot.sh

Emits a .tslog file per run (bench-results/aot-cycle-<MODE>-<TS>.tslog)
with one line per phase boundary: cleanup, cp_golden, node_start,
rpc_wait, warmup, drain, sleep5, real_bench, stop_node. Each line
carries microsecond-precision unix ts via python3 (BSD date lacks ns).
Total cycle ~16 lines per run.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 12: `run-bench-aot.sh` — add bench-window block sentinels

Capture `eth_blockNumber` at warmup/real boundaries; carry as `block=N` extra on `ts_log` lines.

**Files:**
- Modify: `$XLAYER_RETH/run-bench-aot.sh`

- [ ] **Step 1: Add a `safe_block_number` helper near the other helpers**

Add after `wait_for_rpc()`:

```bash
safe_block_number() {
    local result hex
    for attempt in 1 2 3; do
        result=$(curl -s --max-time 2 http://127.0.0.1:8545 -X POST \
            -H 'Content-Type: application/json' \
            -d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}' 2>/dev/null)
        hex=$(echo "$result" | grep -oE '"result":"0x[0-9a-fA-F]+"' | grep -oE '0x[0-9a-fA-F]+')
        if [ -n "$hex" ]; then
            printf '%d\n' "$hex"
            return 0
        fi
        sleep 1
    done
    echo unknown
    return 1
}
```

- [ ] **Step 2: Wire it into the 4 sentinels — modify the existing `ts_log` calls**

Replace these 4 calls in the main bench section:

```bash
ts_log warmup_start
warmup_polycli "$WARMUP_LOG"
ts_log warmup_end

# ...

ts_log real_bench_start
./2-bench.sh 2>&1 | tee "$RESULT_OUT"
BENCH_EXIT=${PIPESTATUS[0]}
ts_log real_bench_end
```

with:

```bash
WARMUP_START_BLOCK=$(safe_block_number)
ts_log warmup_start block=$WARMUP_START_BLOCK
warmup_polycli "$WARMUP_LOG"
WARMUP_END_BLOCK=$(safe_block_number)
ts_log warmup_end block=$WARMUP_END_BLOCK

# ...

REAL_START_BLOCK=$(safe_block_number)
ts_log real_bench_start block=$REAL_START_BLOCK
./2-bench.sh 2>&1 | tee "$RESULT_OUT"
BENCH_EXIT=${PIPESTATUS[0]}
REAL_END_BLOCK=$(safe_block_number)
ts_log real_bench_end block=$REAL_END_BLOCK
```

- [ ] **Step 3: Smoke-test**

```bash
cd /Users/jiezhang/meili/jie.zhang1_dacs_at_okg.com/114/Documents/repository/xlayer-reth
MODE=nojit ./run-bench-aot.sh 2>&1 | tail -10
cat $(ls -t bench-results/aot-cycle-nojit-*.tslog | head -1) | grep -E "warmup_start|warmup_end|real_bench_start|real_bench_end"
```
Expected: 4 lines, each with `block=<number>` (or `block=unknown` if RPC failed).

- [ ] **Step 4: Run parser end-to-end on the new tslog**

```bash
cd /Users/jiezhang/meili/jie.zhang1_dacs_at_okg.com/114/Documents/repository/xlayer-reth
TSLOG=$(ls -t bench-results/aot-cycle-nojit-*.tslog | head -1)
TS=$(basename "$TSLOG" | sed -E 's/.*-nojit-([0-9_]+)\.tslog/\1/')
python3 scripts/parse-bench-timing.py \
    --run-dir bench-results \
    --mode nojit \
    --timestamp "$TS" \
    --json-out /tmp/real-test.json
python3 -c "import json; d=json.load(open('/tmp/real-test.json')); print('warnings:', d['meta']['warnings']); print('wall_clock_keys:', list(d['wall_clock_by_phase'].keys())); print('bench_window:', d['bench_window'])"
```
Expected: warnings is empty `[]`; wall_clock_by_phase has 10 keys; bench_window has 4 block numbers (integers, not None or "unknown").

- [ ] **Step 5: Commit**

```bash
cd /Users/jiezhang/meili/jie.zhang1_dacs_at_okg.com/114/Documents/repository/xlayer-reth
git add run-bench-aot.sh
git commit -m "$(cat <<'EOF'
feat(bench): emit bench window block markers (warmup/real)

Captures eth_blockNumber at warmup_start/end and real_bench_start/end,
appended as `block=N` to the matching ts_log line. Parser uses these
4 block numbers (not wall-clock timestamps) to slice the block_phase_
breakdown stream — immune to clock skew, sleep precision drift.

safe_block_number retries 3× on RPC failure then writes 'unknown',
which parser falls back to wall-clock slicing with a warning.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 13: `run-paid-Nx3.sh` — outer-loop tslog

Add round/mode/cool-down boundary markers to a separate tslog at the outer-loop level.

**Files:**
- Modify: `$XLAYER_RETH/run-paid-Nx3.sh`

- [ ] **Step 1: Add helper + outer tslog declaration**

After `mkdir -p "$OUT_DIR"`, add:

```bash
SUMMARY="$OUT_DIR/SUMMARY.txt"
: > "$SUMMARY"
OUTER_TSLOG="$OUT_DIR/paid-Nx3-cycle.tslog"
ts_log_outer() {
    local ts
    ts=$(python3 -c 'import time; print(f"{time.time():.6f}")')
    echo "ts=$ts phase=$1 ${*:2}" >> "$OUTER_TSLOG"
}

ts_log_outer driver_start rounds=$ROUNDS
```

- [ ] **Step 2: Bracket cool-down and run_one body**

Replace the `run_one()` function:

```bash
run_one() {
    local mode="$1"
    local round="$2"
    local logfile="$OUT_DIR/${mode}-r${round}.log"
    ts_log_outer cool_down_start mode=$mode round=$round
    echo "  [pre-bench cool-down ${COOL_DOWN}s]" | tee -a "$SUMMARY"
    sleep "$COOL_DOWN"
    ts_log_outer cool_down_end mode=$mode round=$round
    ts_log_outer run_start mode=$mode round=$round
    echo "=== mode=$mode round=$round ===" | tee -a "$SUMMARY"
    MODE="$mode" ./run-bench-aot.sh > "$logfile" 2>&1
    ts_log_outer run_end mode=$mode round=$round
    local tps=$(grep -E "^Final TPS:" "$logfile" | tail -1 | awk '{print $3}')
    local errs=$(grep "Num errors" "$logfile" | tail -1 | grep -oE "numErrors=[0-9]+" | cut -d= -f2)
    printf "  %-10s round=%d Final TPS=%-12s errors=%s\n" "$mode" "$round" "$tps" "$errs" | tee -a "$SUMMARY"
}
```

- [ ] **Step 3: Mark driver_end before the aggregation loop**

Find `echo "" | tee -a "$SUMMARY"` near the bottom (just before the aggregation), and add `ts_log_outer driver_end` right before it.

- [ ] **Step 4: Smoke-test (1 round of 3 modes, but use short COOL_DOWN to save time)**

```bash
cd /Users/jiezhang/meili/jie.zhang1_dacs_at_okg.com/114/Documents/repository/xlayer-reth
ROUNDS=1 COOL_DOWN=10 ./run-paid-Nx3.sh 2>&1 | tail -20
ls bench-results/paid-1x3-*/paid-Nx3-cycle.tslog
cat bench-results/paid-1x3-*/paid-Nx3-cycle.tslog | head -20
```
Expected: outer tslog exists; ~10-12 lines covering driver_start, 3× (cool_down_start, cool_down_end, run_start, run_end), driver_end.

- [ ] **Step 5: Commit**

```bash
cd /Users/jiezhang/meili/jie.zhang1_dacs_at_okg.com/114/Documents/repository/xlayer-reth
git add run-paid-Nx3.sh
git commit -m "$(cat <<'EOF'
feat(bench): outer-loop tslog in run-paid-Nx3.sh

Adds paid-Nx3-cycle.tslog at the driver level recording round/mode
boundaries plus cool_down start/end. Independent of per-run tslogs;
used to aggregate ① pipeline-level wall-clock across N rounds × 3 modes.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Phase E — Acceptance, docs, push

### Task 14: End-to-end acceptance run

This is a verification gate, not a commit. Run `ROUNDS=1 ./run-paid-Nx3.sh` and verify the 11 checks from spec §8.4.

- [ ] **Step 1: Reset cool-down to production value + run**

```bash
cd /Users/jiezhang/meili/jie.zhang1_dacs_at_okg.com/114/Documents/repository/xlayer-reth
ROUNDS=1 COOL_DOWN=120 ./run-paid-Nx3.sh 2>&1 | tail -30
```
Expected: ~25 min wall-clock; ends with aggregation table and `Output dir: bench-results/paid-1x3-...`.

- [ ] **Step 2: Locate the run directory**

```bash
RUN_DIR=$(ls -dt /Users/jiezhang/meili/jie.zhang1_dacs_at_okg.com/114/Documents/repository/xlayer-reth/bench-results/paid-1x3-*/ | head -1)
echo "RUN_DIR=$RUN_DIR"
ls "$RUN_DIR"
ls /Users/jiezhang/meili/jie.zhang1_dacs_at_okg.com/114/Documents/repository/xlayer-reth/bench-results/aot-cycle-*-$(basename $RUN_DIR | sed 's/paid-1x3-//').*.tslog 2>&1 | head
```

- [ ] **Step 3: Run parser for all 3 modes**

```bash
cd /Users/jiezhang/meili/jie.zhang1_dacs_at_okg.com/114/Documents/repository/xlayer-reth
# Find timestamps from filenames
for MODE in nojit jit_cold aot; do
    TSLOG=$(ls -t bench-results/aot-cycle-${MODE}-*.tslog | head -1)
    TS=$(basename "$TSLOG" | sed -E "s/.*-${MODE}-([0-9_]+)\.tslog/\1/")
    echo "=== $MODE / $TS ==="
    python3 scripts/parse-bench-timing.py \
        --run-dir bench-results \
        --mode "$MODE" \
        --timestamp "$TS" \
        --json-out "bench-results/cycle-timing-${MODE}-${TS}.json"
done
```

- [ ] **Step 4: Run the 11-point checklist**

```bash
cd /Users/jiezhang/meili/jie.zhang1_dacs_at_okg.com/114/Documents/repository/xlayer-reth
python3 - <<'PYEOF'
import json
import glob
from pathlib import Path

results = {}
for mode in ("nojit", "jit_cold", "aot"):
    candidates = sorted(glob.glob(f"bench-results/cycle-timing-{mode}-*.json"))
    if not candidates:
        print(f"FAIL #1: no cycle-timing-{mode}-*.json"); continue
    p = candidates[-1]
    with open(p) as f:
        results[mode] = json.load(f)

# 1: 3 tslogs (covered by glob above implicitly through json existence)
# 2: 3 JSONs
print(f"#1+#2 ({len(results)}/3 JSON files):", "PASS" if len(results) == 3 else "FAIL")

for mode, d in results.items():
    wc = d["wall_clock_by_phase"]
    # 3: 10 wall_clock fields non-null
    missing = [k for k, v in wc.items() if v is None]
    ok3 = len(missing) == 0 and len(wc) >= 10
    # 4: real_start_block < real_end_block
    bw = d["bench_window"]
    ok4 = bw["real_start_block"] is not None and bw["real_end_block"] is not None and bw["real_start_block"] < bw["real_end_block"]
    # 5: n_real_blocks >= 20
    ok5 = (d.get("chain_blocks_real") or {}).get("n_blocks", 0) >= 20
    # 7,8: AOT only
    if mode == "aot":
        aot = d.get("aot") or {}
        ok7 = (aot.get("dlopen_us") or 0) > 0
        ok8 = (aot.get("n_dylib_loaded") or 0) == 4
    else:
        ok7 = ok8 = True
    # 9: JIT lookup_hits > n_blocks
    if mode in ("jit_cold", "aot"):
        delta = d.get("jit_stats_delta") or {}
        ok9 = delta.get("lookup_hits_delta", 0) > (d.get("chain_blocks_real") or {}).get("n_blocks", 0)
    else:
        ok9 = True
    # 10: conservation
    total = wc.get("total_run_us") or 0
    parts = sum(v for k, v in wc.items() if k != "total_run_us" and v is not None)
    ok10 = total > 0 and abs(parts - total) / total < 0.05
    print(f"  {mode}: #3={ok3} #4={ok4} #5={ok5} #7={ok7} #8={ok8} #9={ok9} #10={ok10}")
    if not ok3: print(f"    missing wall_clock fields: {missing}")

# 6 is informational only — log the cross-mode total mean
total_means = {m: ((d.get("chain_blocks_real") or {}).get("phase_us", {}).get("total", {}) or {}).get("mean") for m, d in results.items()}
print(f"#6 informational — chain total mean per mode: {total_means}")

# 11: visual check
print("#11: run the parser CLI manually and inspect tables; not auto-checked.")
PYEOF
```

Expected: all `True` except possibly #6. If any FAIL, diagnose:
- #1/#2: tslog missing → re-run that mode
- #3: missing wall_clock → ts_log calls dropped in script
- #4: block sentinels failed → `safe_block_number` retries exhausted; check RPC config
- #5: n_real_blocks < 20 → bench too short; check polycli + node both healthy
- #7/#8: AOT cache empty or dlopen log missing → check `aot-cache/` populated
- #9: JIT not hitting cache → check `XLAYER_JIT_ENABLED` propagation
- #10: phase sum != total → check `_phase_durations` arithmetic against tslog

- [ ] **Step 5: Performance overhead check**

Compare `chain_blocks_real.phase_us.total.mean` for `nojit` against an older nojit log (pre-instrumentation):

```bash
cd /Users/jiezhang/meili/jie.zhang1_dacs_at_okg.com/114/Documents/repository/xlayer-reth
OLD_NOJIT=$(ls -t bench-results/aot-node-nojit-*.log | tail -10 | head -1)  # an older log
NEW_NOJIT=$(ls -t bench-results/aot-node-nojit-*.log | head -1)
echo "OLD: $OLD_NOJIT"
echo "NEW: $NEW_NOJIT"
for L in "$OLD_NOJIT" "$NEW_NOJIT"; do
    python3 -c "
import re
ts = []
for line in open('$L'):
    if 'block_phase_breakdown' in line:
        m = re.search(r'total_us=(\d+).*?n_total_txs=(\d+)', line) or re.search(r'n_total_txs=(\d+).*?total_us=(\d+)', line)
        if m:
            try:
                tot, n = int(m.group(1)), int(m.group(2))
                if n >= 100:
                    ts.append(tot)
            except: pass
ts.sort()
if ts:
    print(f'$L: n={len(ts)} p50={ts[len(ts)//2]} mean={sum(ts)//len(ts)}')
else:
    print(f'$L: no heavy blocks')
"
done
```
Expected: nojit mean within ±2% of historical baseline. If > 5% regression, investigate `ts_log` python3 fork cost.

**If all checks pass → proceed to Task 15. If any hard-fail → fix and re-run.**

---

### Task 15: Update `JIT-BENCH-HANDOFF.md`

Add a new section explaining where each metric now lives.

**Files:**
- Modify: `$XLAYER_RETH/JIT-BENCH-HANDOFF.md`

- [ ] **Step 1: Append a new section after the existing §6 (key files)**

Insert before §7 (current git 状态):

```markdown
## 6.5 Instrumentation reference (新增 2026-06-07)

| 数据点 | 来源 | 文件 |
|---|---|---|
| 每个 phase 的 wall-clock | shell `ts_log` | `bench-results/aot-cycle-${MODE}-${TS}.tslog` |
| 每 round × mode + cool_down 的边界 | shell `ts_log_outer` | `bench-results/paid-Nx3-*/paid-Nx3-cycle.tslog` |
| 每块 chain CPU 4 段 + n_seq_txs / n_pool_txs | `xlayer::bench_timing` info! | node log |
| 每秒 JIT counter 累计 | `xlayer::jit::stats` info! | node log |
| AOT dlopen 耗时 + 装载 dylib 数 | `xlayer::jit::aot` info! "AOT store opened" | node log (含 dlopen_us 字段) |
| polycli Final TPS | SA-Bench 自己 emit | `bench-results/aot-result-${MODE}-${TS}.out` |

**Parser 新用法**:
```bash
# 单 run 出 JSON + 4 张终端表
python3 scripts/parse-bench-timing.py \
    --run-dir bench-results \
    --mode aot \
    --timestamp 20260607_143012

# 老用法仍兼容 (legacy --label mode)
python3 scripts/parse-bench-timing.py \
    --label nojit X.log --label aot Y.log
```

**JSON 关键字段**:
- `wall_clock_by_phase.*_us`: 11 个 phase 的微秒数,加 total_run_us
- `bench_window.{warmup,real}_{start,end}_block`: bench 窗口的 4 个 block sentinels
- `chain_blocks_real.phase_us`: real 窗口内,过滤心跳后,4 段 chain CPU 的 p50/p95/mean
- `jit_stats_delta`: real 窗口和 warmup 窗口内的 JIT counter 增量(分开)
- `tps.wall_clock_bench_ratio`: chain block CPU 占 polycli wall-clock 的比例 (典型 ~10%)
- 完整 schema 见 `docs/superpowers/specs/2026-06-07-jit-bench-timing-instrumentation-design.md`

**测试**: `python3 -m unittest scripts.tests.test_parser -v` (7 个 fixture 测试)
```

- [ ] **Step 2: Update §7 (git 状态) to reflect the new commits**

Replace the existing §7 table with:

```markdown
## 7. 当前 git 状态 (2026-06-07 更新)

| 项 | 值 |
|---|---|
| xlayer-reth 分支 | `xl/jit-timing` |
| 包含 commit | periodic JIT stats + bump op-reth + aot dlopen_us + 4 个 shell/parser commit + docs |
| op-reth submodule 分支 | `xl/jit-bench-timing` |
| 包含 commit | 4-phase timing + n_seq_txs/n_pool_txs |
| 是否已 push | **是** (push 在 Task 16 完成) |
```

- [ ] **Step 3: Commit**

```bash
cd /Users/jiezhang/meili/jie.zhang1_dacs_at_okg.com/114/Documents/repository/xlayer-reth
git add JIT-BENCH-HANDOFF.md
git commit -m "$(cat <<'EOF'
docs(bench): update JIT-BENCH-HANDOFF.md with new instrumentation

Adds §6.5 explaining where each timing metric lives, new parser CLI
usage, JSON schema overview, and refreshes §7 git status.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 16: Push to gitlab

Push op-reth first (because the submodule pointer in xlayer-reth depends on it), then xlayer-reth.

- [ ] **Step 1: Pre-push diff check on op-reth**

```bash
cd /Users/jiezhang/meili/jie.zhang1_dacs_at_okg.com/114/Documents/repository/xlayer-reth/deps/optimism/rust/op-reth
git log xl/jit-bench-timing --not origin/xl/jit-bench-timing --stat
```
Expected: 2 commits listed (the 4-phase timing + the n_seq/n_pool split). No `.env`, secrets, or unexpected files in the diff.

- [ ] **Step 2: Push op-reth**

```bash
cd /Users/jiezhang/meili/jie.zhang1_dacs_at_okg.com/114/Documents/repository/xlayer-reth/deps/optimism/rust/op-reth
git push origin xl/jit-bench-timing
```

Note: the op-reth `origin` is `https://github.com/okx/optimism` (verified earlier — not gitlab). Confirm the push lands on the intended remote; if you need gitlab too, add it as a separate remote first.

- [ ] **Step 3: Pre-push diff check on xlayer-reth**

```bash
cd /Users/jiezhang/meili/jie.zhang1_dacs_at_okg.com/114/Documents/repository/xlayer-reth
git log xl/jit-timing --not origin/xl/jit-timing --stat
```
Expected: 8 commits (executor.rs stats + bump submodule + aot dlopen + 2× run-bench-aot.sh + run-paid-Nx3.sh + parser + docs).

- [ ] **Step 4: Push xlayer-reth**

```bash
cd /Users/jiezhang/meili/jie.zhang1_dacs_at_okg.com/114/Documents/repository/xlayer-reth
git push origin xl/jit-timing
```

- [ ] **Step 5: Verify remote**

```bash
cd /Users/jiezhang/meili/jie.zhang1_dacs_at_okg.com/114/Documents/repository/xlayer-reth
git log origin/xl/jit-timing --oneline -10
cd deps/optimism/rust/op-reth
git log origin/xl/jit-bench-timing --oneline -5
```
Expected: remote HEADs match local HEADs.

---

## Plan completion

All of the following must be true:

1. ✓ Task 14 acceptance: all 10 hard checks pass (#6 informational allowed to differ)
2. ✓ Task 14 perf check: nojit overhead < 2%
3. ✓ Both branches pushed (Task 16 Step 5 verification)
4. ✓ Docs updated and committed (Task 15)
5. ✓ One manual `parse-bench-timing.py --run-dir bench-results --mode aot --timestamp <latest>` produces 4 tables + JSON, fields look sane

If any item fails, do not declare the plan done — fix and re-verify.

---

## Notes for the executor

- **TMPDIR for cargo**: The build commands inherit `TMPDIR` from the shell. If you see "macOS sandbox" or "tempfile::tempdir() failed" errors during builds initiated from inside this session, prefix builds with `TMPDIR="$XLAYER_RETH/target/test_tmp"` (the same trick `run-bench-aot.sh` already uses for runtime).
- **Submodule pointer**: every time you commit in op-reth, the parent xlayer-reth's submodule pointer will show as `M`. Task 9 commits this delta explicitly. Don't accidentally include the bumped submodule pointer in unrelated commits.
- **Co-author footer**: each commit message ends with `Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>` per repo convention; preserve this on every commit.
- **Stop conditions**: if Task 8 build fails on `info.cumulative_tx_count` type, add `as u64` casts and rebuild — don't switch types upstream. If Task 14 perf check shows > 5% regression, investigate `ts_log`'s `python3` fork overhead and switch to bash `EPOCHREALTIME` (bash 5+).
