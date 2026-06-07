#!/usr/bin/env python3
"""Parse xlayer-reth bench timing logs and emit per-phase percentile tables.

Usage:
    parse-bench-timing.py <node.log> [<node.log> ...]
    parse-bench-timing.py --label nojit nojit.log --label aot aot.log

Parses two kinds of log lines:
  - target=xlayer::bench_timing, message=block_phase_breakdown
      → per-block OpPayloadBuilder phase breakdown
      (pre_exec_us, seq_txs_us, pool_exec_us, finish_us, total_us)
  - target=xlayer::jit::stats, message=jit_periodic_stats
      → final snapshot only (last observed values per file)

For each --label group (or each file if no labels), prints:
  - per-phase p50 / p95 / mean / count
  - tx counts (n_pool_txs avg)
  - JIT stats final snapshot
"""
from __future__ import annotations

import argparse
import re
import statistics
import sys
from pathlib import Path

# Capture key=value where value is digits or quoted string. We only care about
# numeric phase metrics, so digit-only is sufficient.
KV_RE = re.compile(r"\b([a-z_][a-z0-9_]*)=(\d+)")

# Strip terminal ANSI escape codes (color, style) before regex matching.
# Tracing emits these when stdout is a TTY-attached file; the codes break
# the \b word boundary used in KV_RE.
ANSI_RE = re.compile(r"\x1b\[[0-9;]*[A-Za-z]")

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


# --- node log reader -------------------------------------------------------
# Tracing emits RFC-3339 timestamps prefix: "2026-06-01T12:02:00.000000Z  INFO ..."
NODE_TS_RE = re.compile(r"^(\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d+Z)")


def _parse_node_ts(line: str) -> float | None:
    m = NODE_TS_RE.match(line)
    if not m:
        return None
    from datetime import datetime
    try:
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

    # Minimal-schema guards: drop truncated lines so downstream callers (process_run,
    # _jit_stats_delta) can index essential fields without defensive .get() everywhere.
    # KV_RE is digit-only by design; string-valued fields like "dir=..." on AOT lines
    # are intentionally dropped — only numeric metrics flow downstream.
    BLOCK_REQUIRED = ("block", "n_total_txs")
    JIT_REQUIRED = ("compile_ok", "lookup_hits", "lookup_misses", "evictions")
    with path.open(errors="replace") as f:
        for raw in f:
            line = ANSI_RE.sub("", raw)
            ts = _parse_node_ts(line)
            if "block_phase_breakdown" in line:
                kv = {k: int(v) for k, v in KV_RE.findall(line)}
                if all(f in kv for f in BLOCK_REQUIRED):
                    kv["ts"] = ts
                    blocks.append(kv)
            elif "jit_periodic_stats" in line:
                kv = {k: int(v) for k, v in KV_RE.findall(line)}
                if all(f in kv for f in JIT_REQUIRED):
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
    """Parse polycli output for Final TPS + numErrors. Missing -> None."""
    if not path.exists():
        return {"final_tps": None, "num_errors": None}
    text = path.read_text(errors="replace")
    tps_m = POLYCLI_TPS_RE.search(text)
    err_m = POLYCLI_ERR_RE.search(text)
    return {
        "final_tps": float(tps_m.group(1)) if tps_m else None,
        "num_errors": int(err_m.group(1)) if err_m else None,
    }


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


def _chain_blocks_real(node_blocks: list[dict], window: dict, min_block_txs: int = 100) -> "dict | None":
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
    s = next((sn for sn in jit_snapshots if sn["ts"] and sn["ts"] >= real_start["ts"]), None)
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


PHASE_FIELDS = (
    "pre_exec_us",
    "seq_txs_us",
    "pool_exec_us",
    "finish_us",
    "total_us",
)
TX_FIELDS = ("n_pool_txs", "n_seq_txs", "n_total_txs")
JIT_FIELDS = (
    "resident",
    "compile_ok",
    "compile_err",
    "dispatched",
    "lookup_hits",
    "lookup_misses",
    "pending",
    "evictions",
    "code_bytes",
)


def parse_log(path: Path) -> dict:
    """Parse one log file, returning {phase_rows, tx_rows, jit_final}."""
    phase_rows: list[dict[str, int]] = []
    tx_rows: list[dict[str, int]] = []
    jit_final: dict[str, int] = {}

    if not path.exists():
        print(f"warn: {path} not found", file=sys.stderr)
        return {"phase_rows": phase_rows, "tx_rows": tx_rows, "jit_final": jit_final}

    with path.open(errors="replace") as f:
        for raw_line in f:
            line = ANSI_RE.sub("", raw_line)
            if "block_phase_breakdown" in line:
                kv = dict(KV_RE.findall(line))
                phase_rows.append({k: int(kv[k]) for k in PHASE_FIELDS if k in kv})
                tx_rows.append({k: int(kv[k]) for k in TX_FIELDS if k in kv})
            elif "jit_periodic_stats" in line:
                kv = dict(KV_RE.findall(line))
                # Keep updating; we'll keep the last one
                for k in JIT_FIELDS:
                    if k in kv:
                        jit_final[k] = int(kv[k])
    return {"phase_rows": phase_rows, "tx_rows": tx_rows, "jit_final": jit_final}


def merge(parsed_list: list[dict]) -> dict:
    """Merge multiple parsed-log dicts into one (concatenate rows, keep last jit)."""
    merged = {"phase_rows": [], "tx_rows": [], "jit_final": {}}
    for p in parsed_list:
        merged["phase_rows"].extend(p["phase_rows"])
        merged["tx_rows"].extend(p["tx_rows"])
        merged["jit_final"].update(p["jit_final"])
    return merged


def summarize(label: str, data: dict) -> None:
    rows = data["phase_rows"]
    if not rows:
        print(f"\n=== {label} ===  (no block_phase_breakdown lines)")
        return
    print(f"\n=== {label} ===  N blocks = {len(rows)}")
    print(f"{'phase':<14} {'p50':>10} {'p95':>10} {'mean':>10} {'n':>6}")
    for f in PHASE_FIELDS:
        v = sorted(r[f] for r in rows if f in r)
        if not v:
            continue
        n = len(v)
        p50 = v[n // 2]
        p95 = v[min(n - 1, int(n * 0.95))]
        mean = sum(v) // n
        print(f"{f:<14} {p50:>10} {p95:>10} {mean:>10} {n:>6}")

    # tx counts
    tx_rows = data["tx_rows"]
    if tx_rows:
        for f in TX_FIELDS:
            v = [r[f] for r in tx_rows if f in r]
            if v:
                print(f"  {f:<12} avg={sum(v)/len(v):8.1f}  max={max(v):>6}")

    # JIT snapshot
    jit = data["jit_final"]
    if jit:
        print("  JIT (last snapshot):")
        for f in JIT_FIELDS:
            if f in jit:
                print(f"    {f:<14} {jit[f]:>12}")


def cmp_table(groups: list[tuple[str, dict]]) -> None:
    """Side-by-side comparison of phase medians and Δ% vs first group."""
    if len(groups) < 2:
        return
    base_label, base_data = groups[0]
    base_med = {}
    for f in PHASE_FIELDS:
        v = sorted(r[f] for r in base_data["phase_rows"] if f in r)
        if v:
            base_med[f] = v[len(v) // 2]
    if not base_med:
        return

    print(f"\n=== Comparison (Δ% vs {base_label}, based on p50) ===")
    header = f"{'phase':<14}" + "".join(f"{lbl:>14}" for lbl, _ in groups) + \
        "".join(f"{lbl+'_Δ%':>10}" for lbl, _ in groups[1:])
    print(header)
    for f in PHASE_FIELDS:
        if f not in base_med:
            continue
        line = f"{f:<14}"
        meds = []
        for _, data in groups:
            v = sorted(r[f] for r in data["phase_rows"] if f in r)
            if v:
                m = v[len(v) // 2]
                meds.append(m)
                line += f"{m:>14}"
            else:
                meds.append(None)
                line += f"{'n/a':>14}"
        for m in meds[1:]:
            if m is None or base_med[f] == 0:
                line += f"{'n/a':>10}"
            else:
                d = (m / base_med[f] - 1) * 100
                line += f"{d:>9.1f}%"
        print(line)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--label", action="append", default=[],
                    help="Group label; pair with following file(s) until next --label")
    ap.add_argument("files", nargs="*", help="Log file(s)")
    args = ap.parse_args()

    # Re-parse argv to interleave labels and files (argparse's action="append"
    # doesn't preserve interleaving with positional args, so do it manually).
    groups: list[tuple[str, list[Path]]] = []
    current_label = None
    current_files: list[Path] = []
    raw = sys.argv[1:]
    i = 0
    while i < len(raw):
        a = raw[i]
        if a == "--label":
            if current_label is not None:
                groups.append((current_label, current_files))
            current_label = raw[i + 1]
            current_files = []
            i += 2
        else:
            current_files.append(Path(a))
            i += 1
    if current_label is not None:
        groups.append((current_label, current_files))
    if not groups:
        # No --label given; treat each file as its own group
        for f in args.files:
            groups.append((Path(f).name, [Path(f)]))

    if not groups:
        ap.print_help()
        sys.exit(1)

    parsed_groups: list[tuple[str, dict]] = []
    for label, files in groups:
        parsed = merge([parse_log(p) for p in files])
        parsed_groups.append((label, parsed))
        summarize(label, parsed)

    cmp_table(parsed_groups)


if __name__ == "__main__":
    main()
