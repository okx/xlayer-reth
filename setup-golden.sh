#!/usr/bin/env bash
# One-time setup: start dev node from genesis, run hardhat deploy, save the
# resulting state DB as `golden-state/`. Once this exists, run-bench-aot.sh
# restores from it instead of redoing hardhat each mode (saves ~12 min/mode).
#
# Idempotent: if golden-state/ already exists, refuses to overwrite. Use
# `--force` to wipe.
set -euo pipefail

XLAYER_DIR="/Users/jiezhang/meili/jie.zhang1_dacs_at_okg.com/114/Documents/repository/xlayer-reth"
SA_DIR="/Users/jiezhang/meili/jie.zhang1_dacs_at_okg.com/114/Documents/SA-Benchmark"
GOLDEN_DIR="$XLAYER_DIR/golden-state"
RESULT_DIR="$XLAYER_DIR/bench-results"
TS=$(date +"%Y%m%d_%H%M%S")
NODE_LOG="$RESULT_DIR/setup-golden-node-$TS.log"
DEPLOY_LOG="$RESULT_DIR/setup-golden-deploy-$TS.log"
mkdir -p "$RESULT_DIR"

FORCE=0
[ "${1:-}" = "--force" ] && FORCE=1

if [ -d "$GOLDEN_DIR" ] && [ "$FORCE" != "1" ]; then
    echo "$GOLDEN_DIR already exists. Pass --force to overwrite."
    exit 1
fi
rm -rf "$GOLDEN_DIR"

cd "$XLAYER_DIR"

echo "=== 1) cleanup any prior node ==="
pkill -9 -f xlayer-reth-jit 2>/dev/null || true
sleep 2
rm -rf "$XLAYER_DIR/jit-data" "$XLAYER_DIR/jit-logs"

echo "=== 2) start dev node (JIT off, simpler for deploy) ==="
tmp_override="$XLAYER_DIR/target/test_tmp"
mkdir -p "$tmp_override"
TMPDIR="$tmp_override" XLAYER_JIT_ENABLED=0 XLAYER_AOT_DIR="" \
    BLOCK_TIME="${BLOCK_TIME:-1s}" \
    ./run.sh > "$NODE_LOG" 2>&1 &
NODE_PID=$!
echo "node PID=$NODE_PID, log=$NODE_LOG"

echo "=== 3) wait for RPC ==="
for i in $(seq 1 60); do
    if curl -s http://127.0.0.1:8545 -X POST \
         -H 'Content-Type: application/json' \
         -d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}' 2>/dev/null \
       | grep -q result; then
        echo "RPC ready (${i}s)"
        break
    fi
    sleep 1
done

echo "=== 4) run hardhat deploy (this is the slow ~12-min step) ==="
export PATH="/Users/jiezhang/.nvm/versions/node/v20.20.2/bin:$PATH"
cd "$SA_DIR"
sed -i '' 's/^DEPLOY_CONTRACTS=.*/DEPLOY_CONTRACTS=true/' .env
T0=$(date +%s)
if ! npx hardhat run scripts/deploy/deploy.ts --network local --no-compile > "$DEPLOY_LOG" 2>&1; then
    echo "ERROR: deploy failed. Last 30 lines:"
    tail -30 "$DEPLOY_LOG"
    sed -i '' 's/^DEPLOY_CONTRACTS=.*/DEPLOY_CONTRACTS=false/' .env
    kill -9 "$NODE_PID" 2>/dev/null
    exit 1
fi
sed -i '' 's/^DEPLOY_CONTRACTS=.*/DEPLOY_CONTRACTS=false/' .env
T1=$(date +%s)
echo "deploy completed in $((T1 - T0))s"

# Sanity check: deploy.ts prints contract addresses
if ! grep -q "ENTRYPOINT:" "$DEPLOY_LOG"; then
    echo "ERROR: deploy log missing ENTRYPOINT marker, deploy may have silently failed"
    tail -30 "$DEPLOY_LOG"
    kill -9 "$NODE_PID" 2>/dev/null
    exit 1
fi
echo "deploy log markers:"
grep -E "ENTRYPOINT|HELPER|PAY|TEST_ERC20|PAYABLE_ACCOUNT" "$DEPLOY_LOG" | head -10

echo "=== 5) flush mempool + give chain a few seconds to settle ==="
sleep 5

echo "=== 6) stop node cleanly ==="
kill "$NODE_PID" 2>/dev/null || true
sleep 5
pkill -9 -f xlayer-reth-jit 2>/dev/null || true
sleep 2

echo "=== 7) snapshot state ==="
cd "$XLAYER_DIR"
if [ ! -d "$XLAYER_DIR/jit-data" ]; then
    echo "ERROR: jit-data not found after deploy"
    exit 1
fi
cp -R "$XLAYER_DIR/jit-data" "$GOLDEN_DIR"
echo "Saved: $GOLDEN_DIR"
echo "Size:  $(du -sh "$GOLDEN_DIR" | cut -f1)"

echo "=== 8) cleanup working dir ==="
rm -rf "$XLAYER_DIR/jit-data" "$XLAYER_DIR/jit-logs"

echo ""
echo "================================================================="
echo "Golden state setup complete."
echo "  Path:  $GOLDEN_DIR"
echo "  Deploy log: $DEPLOY_LOG"
echo "  Node log:   $NODE_LOG"
echo ""
echo "Next: ./run-paid-Nx3.sh (will auto-restore golden state per mode)"
echo "================================================================="
