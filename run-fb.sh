#!/usr/bin/env bash
# X-Layer dev node launcher WITH flashblocks enabled (Mode B variant of run.sh).
#
# Differences from run.sh:
#   - --flashblocks.enabled (use FlashblocksBuilder instead of OpPayloadBuilder)
#   - --flashblocks.block-time defaults to 250ms (4 sub-blocks per 1s parent block)
#   - DATADIR/PORT/FLASHBLOCKS_PORT use separate defaults to avoid clashing
#     with a parallel run.sh instance.
#
# Usage:
#   ./run-fb.sh                       # JIT on, flashblocks 250ms
#   XLAYER_JIT_ENABLED=0 ./run-fb.sh  # nojit, flashblocks 250ms
#   FLASHBLOCKS_BLOCK_TIME=500 ./run-fb.sh
set -euo pipefail

PORT="${PORT:-8545}"
AUTH_PORT="${AUTH_PORT:-8551}"
P2P_PORT="${P2P_PORT:-30303}"
FB_WS_PORT="${FB_WS_PORT:-1111}"
GAS_LIMIT="${GAS_LIMIT:-600000000}"
BLOCK_TIME="${BLOCK_TIME:-1s}"
FB_BLOCK_TIME_MS="${FB_BLOCK_TIME_MS:-250}"

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="${SCRIPT_DIR}"
DATADIR="${DATADIR:-${PROJECT_ROOT}/jit-data-fb}"

# Strip OTEL env vars inherited from Claude Code.
unset OTEL_METRICS_EXPORTER OTEL_LOGS_EXPORTER OTEL_TRACES_EXPORTER \
      OTEL_EXPORTER_OTLP_PROTOCOL OTEL_EXPORTER_OTLP_ENDPOINT \
      OTEL_EXPORTER_OTLP_METRICS_ENDPOINT OTEL_EXPORTER_OTLP_LOGS_ENDPOINT \
      OTEL_EXPORTER_OTLP_TRACES_ENDPOINT OTEL_EXPORTER_OTLP_HEADERS \
      OTEL_EXPORTER_OTLP_METRICS_TEMPORALITY_PREFERENCE OTEL_SERVICE_NAME

export XLAYER_JIT_ENABLED="${XLAYER_JIT_ENABLED:-1}"

BIN="${SCRIPT_DIR}/bin/xlayer-reth-jit"
GENESIS="${SCRIPT_DIR}/bench-data/bench-opstack-genesis-nojovian.json"

if [ "$XLAYER_JIT_ENABLED" = "1" ]; then
    echo ">>> [FB] Starting with JIT runtime ENABLED + flashblocks (${FB_BLOCK_TIME_MS}ms × ~$((1000 / FB_BLOCK_TIME_MS)))"
else
    echo ">>> [FB] Starting with JIT runtime DISABLED + flashblocks (${FB_BLOCK_TIME_MS}ms × ~$((1000 / FB_BLOCK_TIME_MS)))"
fi

if [ ! -f "$BIN" ]; then
    echo "Error: binary not found at $BIN" >&2
    exit 1
fi

mkdir -p "$DATADIR"

export RETH_DEV_GAS_LIMIT="$GAS_LIMIT"
export RETH_DEV_BASE_FEE_MAX_CHANGE_DENOMINATOR=1000000
export RETH_DEV_BASE_FEE_ELASTICITY_MULTIPLIER=4

exec "$BIN" node \
    --datadir "$DATADIR" \
    --dev \
    --dev.block-time "$BLOCK_TIME" \
    --chain "$GENESIS" \
    --builder.gaslimit "$GAS_LIMIT" \
    --txpool.pending-max-count 10000000 \
    --txpool.pending-max-size 10000 \
    --txpool.basefee-max-count 10000000 \
    --txpool.basefee-max-size 10000 \
    --txpool.queued-max-count 10000000 \
    --txpool.queued-max-size 10000 \
    --txpool.blobpool-max-count 10000000 \
    --txpool.blobpool-max-size 10000 \
    --txpool.max-account-slots 10000000 \
    --xlayer.sequencer-mode \
    --flashblocks.enabled \
    --flashblocks.block-time "$FB_BLOCK_TIME_MS" \
    --flashblocks.port "$FB_WS_PORT" \
    --http \
    --http.port "$PORT" \
    --http.api eth,debug,net,web3,txpool \
    --authrpc.port "$AUTH_PORT" \
    --discovery.port "$P2P_PORT" \
    --rpc.gascap 1500000000 \
    --ipcdisable \
    --log.stdout.filter "info,engine::tree::payload_validator=debug,payload_builder=debug"
