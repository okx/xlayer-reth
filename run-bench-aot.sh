#!/usr/bin/env bash
# 3-mode bench wrapper for JIT-AOT comparison.
#
# Usage:
#   MODE=nojit                  ./run-bench-aot.sh     # Pure interpreter
#   MODE=jit_cold               ./run-bench-aot.sh     # JIT, no persistence (in-memory only)
#   MODE=aot AOT_PREWARMUP=1    ./run-bench-aot.sh     # First-run AOT: warm cache to disk, then bench
#   MODE=aot                    ./run-bench-aot.sh     # Subsequent AOT: load existing cache, then bench
#
# Env:
#   AOT_DIR     — directory for persistent dylib cache (default: $HOME/.xlayer-aot-bench)
#   BLOCK_TIME  — passed to run.sh (default: 1s)
set -euo pipefail

MODE="${MODE:-jit_cold}"
XLAYER_DIR="/Users/jiezhang/meili/jie.zhang1_dacs_at_okg.com/114/Documents/repository/xlayer-reth"
SA_DIR="/Users/jiezhang/meili/jie.zhang1_dacs_at_okg.com/114/Documents/SA-Benchmark"
PROJECT_ROOT="/Users/jiezhang/meili/jie.zhang1_dacs_at_okg.com/114/Documents/repository/xlayer-reth"
RESULT_DIR="$PROJECT_ROOT/bench-results"
AOT_DIR="${AOT_DIR:-$PROJECT_ROOT/aot-cache}"
mkdir -p "$RESULT_DIR"

case "$MODE" in
    nojit)    JIT_FLAG=0; AOT_DIR_ARG="" ;;
    jit_cold) JIT_FLAG=1; AOT_DIR_ARG="" ;;
    aot)      JIT_FLAG=1; AOT_DIR_ARG="$AOT_DIR" ;;
    *)        echo "MODE must be one of: nojit, jit_cold, aot" >&2; exit 1 ;;
esac

TIMESTAMP=$(date +"%Y%m%d_%H%M%S")
NODE_LOG="$RESULT_DIR/aot-node-${MODE}-${TIMESTAMP}.log"
RESULT_OUT="$RESULT_DIR/aot-result-${MODE}-${TIMESTAMP}.out"
DEPLOY_LOG="$RESULT_DIR/aot-deploy-${MODE}-${TIMESTAMP}.log"
WARMUP_LOG="$RESULT_DIR/aot-warmup-${MODE}-${TIMESTAMP}.out"
TSLOG="$RESULT_DIR/aot-cycle-${MODE}-${TIMESTAMP}.tslog"
ts_log() {
    # phase=$1 [k=v ...]
    local ts
    ts=$(python3 -c 'import time; print(f"{time.time():.6f}")')
    echo "ts=$ts phase=$1 ${*:2}" >> "$TSLOG"
}

# ============================================================================
# Helper functions
# ============================================================================

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
    # If a golden state snapshot exists, restore it instead of starting from
    # genesis. This skips the ~12-min hardhat recompile + deploy step that
    # would otherwise run once per mode.
    if [ -d "$PROJECT_ROOT/golden-state" ]; then
        echo "[start_node] restoring golden-state → jit-data"
        ts_log cp_golden_start
        cp -R "$PROJECT_ROOT/golden-state" "$PROJECT_ROOT/jit-data"
        ts_log cp_golden_end
    fi
    cd "$XLAYER_DIR"
    # Force TMPDIR to a writable project-local location. macOS sandbox blocks the
    # default /var/folders/... TMPDIR when the binary is launched from inside
    # Claude Code, breaking revmc's tempfile::tempdir() calls for LLVM linking.
    local tmp_override="$XLAYER_DIR/target/test_tmp"
    mkdir -p "$tmp_override" 2>/dev/null || true
    ts_log node_start
    TMPDIR="$tmp_override" XLAYER_JIT_ENABLED=$jit_flag XLAYER_AOT_DIR="$aot_dir" \
        BLOCK_TIME="${BLOCK_TIME:-1s}" \
        ./run.sh > "$logfile" 2>&1 &
    echo $!
}

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

stop_node() {
    local pid="$1"
    kill "$pid" 2>/dev/null || true
    sleep 3
    pkill -9 -f xlayer-reth-jit 2>/dev/null || true
}

deploy_contracts() {
    local logfile="$1"
    export PATH="/Users/jiezhang/.nvm/versions/node/v20.20.2/bin:$PATH"
    cd "$SA_DIR"
    sed -i '' 's/^DEPLOY_CONTRACTS=.*/DEPLOY_CONTRACTS=true/' .env
    if ! npx hardhat run scripts/deploy/deploy.ts --network local --no-compile > "$logfile" 2>&1; then
        echo "ERROR: yarn deploy failed" >&2
        tail -30 "$logfile" >&2
        return 1
    fi
    sed -i '' 's/^DEPLOY_CONTRACTS=.*/DEPLOY_CONTRACTS=false/' .env
    sleep 5
}

warmup_polycli() {
    local logfile="$1"
    cd "$SA_DIR"
    local orig=$(grep '^TOTAL_UOP=' .env | cut -d= -f2)
    sed -i '' 's/^TOTAL_UOP=.*/TOTAL_UOP=2000/' .env
    export PATH="$HOME/go/bin:$PATH"
    ./2-bench.sh > "$logfile" 2>&1 || true
    sed -i '' "s/^TOTAL_UOP=.*/TOTAL_UOP=$orig/" .env
}

drain_txpool() {
    for drain in $(seq 1 60); do
        local pool=$(curl -s http://127.0.0.1:8545 -X POST -H 'Content-Type: application/json' \
            -d '{"jsonrpc":"2.0","method":"txpool_status","params":[],"id":1}' 2>/dev/null)
        local pend_hex=$(echo "$pool" | grep -oE '"pending":"0x[0-9a-fA-F]+"' | grep -oE '0x[0-9a-fA-F]+' || echo "0x0")
        local queu_hex=$(echo "$pool" | grep -oE '"queued":"0x[0-9a-fA-F]+"' | grep -oE '0x[0-9a-fA-F]+' || echo "0x0")
        local pend=$((pend_hex))
        local queu=$((queu_hex))
        if [ "$pend" -eq 0 ] && [ "$queu" -eq 0 ]; then
            echo "txpool drained after ${drain}s"
            return 0
        fi
        sleep 1
    done
}

# ============================================================================
# Step A: AOT prewarmup (mode=aot first run only)
# ============================================================================

if [ "$MODE" = "aot" ] && [ "${AOT_PREWARMUP:-0}" = "1" ]; then
    echo ""
    echo "================================================================="
    echo "[aot prewarmup] populating $AOT_DIR with JIT compilations"
    echo "================================================================="
    rm -rf "$AOT_DIR"
    mkdir -p "$AOT_DIR"

    PREWARM_LOG="$RESULT_DIR/aot-prewarmup-${TIMESTAMP}.log"
    NODE_PID=$(start_node 1 "$AOT_DIR" "$PREWARM_LOG")
    echo "prewarmup node PID=$NODE_PID, log=$PREWARM_LOG"
    wait_for_rpc

    deploy_contracts "$RESULT_DIR/aot-prewarmup-deploy-${TIMESTAMP}.log"
    echo "running warmup polycli (2000 UOP)..."
    warmup_polycli "$RESULT_DIR/aot-prewarmup-warmup-${TIMESTAMP}.out"
    sleep 10  # give workers time to flush compiled artifacts to disk

    stop_node "$NODE_PID"
    sleep 3
    ART_COUNT=$(ls "$AOT_DIR"/*.so 2>/dev/null | wc -l | tr -d ' ')
    MANIFEST_COUNT=$(ls "$AOT_DIR"/*.manifest.json 2>/dev/null | wc -l | tr -d ' ')
    echo "AOT cache populated: $ART_COUNT dylib + $MANIFEST_COUNT manifests in $AOT_DIR"
    if [ "$ART_COUNT" -eq 0 ]; then
        echo "WARN: AOT prewarmup produced no artifacts; subsequent aot run will fall back to JIT cold"
    fi
fi

# ============================================================================
# Step B: real measurement
# ============================================================================

echo ""
echo "================================================================="
echo "[$MODE] real measurement run (jit=$JIT_FLAG aot_dir='$AOT_DIR_ARG')"
echo "================================================================="

ts_log run_start mode=$MODE
NODE_PID=$(start_node $JIT_FLAG "$AOT_DIR_ARG" "$NODE_LOG")
echo "node PID=$NODE_PID, log=$NODE_LOG"
wait_for_rpc

if [ "$MODE" = "aot" ] && [ -d "$AOT_DIR" ]; then
    # Probe node log for the "AOT store opened loaded=N" line
    sleep 1
    if grep -q "AOT store opened" "$NODE_LOG" 2>/dev/null; then
        echo "AOT preload OK: $(grep "AOT store opened" "$NODE_LOG" | tail -1)"
    else
        echo "WARN: did not see 'AOT store opened' in node log"
    fi
fi

# Skip hardhat deploy when a golden state snapshot is available — contracts
# are already deployed in that state and restored above.
if [ -d "$PROJECT_ROOT/golden-state" ]; then
    echo "[bench] golden-state present → skipping hardhat deploy"
else
    deploy_contracts "$DEPLOY_LOG"
    echo "deploy ok"
fi

echo "=== [$MODE] JIT warmup (2000 UOP) ==="
WARMUP_START_BLOCK=$(safe_block_number)
ts_log warmup_start block=$WARMUP_START_BLOCK
warmup_polycli "$WARMUP_LOG"
WARMUP_END_BLOCK=$(safe_block_number)
ts_log warmup_end block=$WARMUP_END_BLOCK

echo "=== [$MODE] drain txpool ==="
ts_log drain_start
drain_txpool
ts_log drain_end
ts_log sleep5_start
sleep 5
ts_log sleep5_end

echo "=== [$MODE] real bench ==="
cd "$SA_DIR"
REAL_START_BLOCK=$(safe_block_number)
ts_log real_bench_start block=$REAL_START_BLOCK
./2-bench.sh 2>&1 | tee "$RESULT_OUT"
BENCH_EXIT=${PIPESTATUS[0]}
REAL_END_BLOCK=$(safe_block_number)
ts_log real_bench_end block=$REAL_END_BLOCK

ts_log stop_node_start
stop_node "$NODE_PID"
ts_log stop_node_end
ts_log run_end

echo ""
echo "================================================================="
echo "[$MODE] DONE"
echo "================================================================="
echo "Bench result:    $RESULT_OUT"
echo "Node log:        $NODE_LOG"
echo "Warmup log:      $WARMUP_LOG"
[ -n "$AOT_DIR_ARG" ] && echo "AOT dir:         $AOT_DIR_ARG ($(ls "$AOT_DIR_ARG"/*.so 2>/dev/null | wc -l | tr -d ' ') dylib)"
echo ""
echo "=== Final TPS ==="
grep -iE "Final TPS|tps=" "$RESULT_OUT" | tail -3

# Pull AOT stats from node log if available
if grep -q "AOT store opened" "$NODE_LOG" 2>/dev/null; then
    echo ""
    echo "=== AOT stats from node log ==="
    grep -E "AOT store opened|JIT runtime enabled" "$NODE_LOG" | head -5
fi

exit $BENCH_EXIT
