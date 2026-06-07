#!/usr/bin/env bash
# Run N rounds × {nojit, jit_cold, aot} of the SA-Bench paid-contract path.
# Skips AOT prewarmup if cache is already populated.
# Output: bench-results/paid-Nx3-$TS/{MODE-rN.log, SUMMARY.txt}
#
# Usage:
#   ROUNDS=6 ./run-paid-Nx3.sh    # 6 rounds × 3 modes = 18 runs
#   ROUNDS=3 ./run-paid-Nx3.sh    # default
set -uo pipefail

ROUNDS="${ROUNDS:-3}"
XLAYER_DIR="/Users/jiezhang/meili/jie.zhang1_dacs_at_okg.com/114/Documents/repository/xlayer-reth"
PROJECT_ROOT="/Users/jiezhang/meili/jie.zhang1_dacs_at_okg.com/114/Documents/repository/xlayer-reth"
AOT_DIR="$PROJECT_ROOT/aot-cache"
TS=$(date +"%Y%m%d_%H%M%S")
OUT_DIR="$PROJECT_ROOT/bench-results/paid-${ROUNDS}x3-$TS"
mkdir -p "$OUT_DIR"
SUMMARY="$OUT_DIR/SUMMARY.txt"
: > "$SUMMARY"
OUTER_TSLOG="$OUT_DIR/paid-Nx3-cycle.tslog"
ts_log_outer() {
    # phase=$1 [k=v ...]
    local ts
    ts=$(python3 -c 'import time; print(f"{time.time():.6f}")')
    echo "ts=$ts phase=$1 ${*:2}" >> "$OUTER_TSLOG"
}

cd "$XLAYER_DIR"

ts_log_outer driver_start rounds=$ROUNDS

COOL_DOWN="${COOL_DOWN:-60}"
run_one() {
    local mode="$1"
    local round="$2"
    local logfile="$OUT_DIR/${mode}-r${round}.log"
    # Pre-bench thermal cool-down. Moving this BEFORE the bench guarantees
    # every run (including the first) starts from a consistent thermal /
    # OS-cache state — otherwise mode 1 runs hot off setup, modes 2-N run
    # cool off the prior 60-300s sleep, biasing comparison.
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

# AOT prewarmup only if cache empty
ART_COUNT=$(ls "$AOT_DIR"/*.so 2>/dev/null | wc -l | tr -d ' ')
if [ "$ART_COUNT" -ge 3 ]; then
    echo "=== AOT cache already populated ($ART_COUNT dylib) — skipping prewarmup ===" | tee -a "$SUMMARY"
else
    echo "=== AOT prewarmup (cache has $ART_COUNT dylib, need ≥3) ===" | tee -a "$SUMMARY"
    AOT_PREWARMUP=1 MODE=aot ./run-bench-aot.sh > "$OUT_DIR/aot-prewarmup.log" 2>&1
    ART_COUNT=$(ls "$AOT_DIR"/*.so 2>/dev/null | wc -l | tr -d ' ')
    echo "  AOT artifacts after prewarmup: $ART_COUNT dylib" | tee -a "$SUMMARY"
fi
echo "" | tee -a "$SUMMARY"

# N rounds × 3 modes, interleaved
for r in $(seq 1 "$ROUNDS"); do
    for m in nojit jit_cold aot; do
        run_one "$m" "$r"
    done
done

ts_log_outer driver_end
echo "" | tee -a "$SUMMARY"
echo "=== Aggregation (n=$ROUNDS per mode) ===" | tee -a "$SUMMARY"
for m in nojit jit_cold aot; do
    grep -E "^\s*$m " "$SUMMARY" | grep -oE "Final TPS=[0-9.]+" | cut -d= -f2 > /tmp/tps_${m}.txt
    python3 - <<PYEOF | tee -a "$SUMMARY"
import statistics
with open('/tmp/tps_${m}.txt') as f:
    v = [float(x) for x in f.read().split() if x]
if not v:
    print('${m}: no data')
else:
    mean = statistics.mean(v)
    std = statistics.stdev(v) if len(v) > 1 else 0.0
    cov = (std/mean*100) if mean else 0.0
    print(f'${m}: n={len(v)} mean={mean:.1f} std={std:.1f} cov={cov:.1f}% min={min(v):.1f} max={max(v):.1f}')
PYEOF
done

echo ""
echo "Output dir: $OUT_DIR"
echo "Summary:    $SUMMARY"
