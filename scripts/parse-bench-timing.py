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
