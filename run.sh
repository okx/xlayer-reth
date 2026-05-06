#!/usr/bin/env bash
# X-Layer dev node launcher matching SA-Bench JIT comprehensive test config.
# Usage:
#   ./run.sh                       # JIT enabled (default)
#   XLAYER_JIT_ENABLED=0 ./run.sh  # JIT runtime disabled (interpret-only)
#
# Same JIT binary in both modes — XLAYER_JIT_ENABLED gates the runtime backend.
set -euo pipefail

PORT="${PORT:-8545}"
AUTH_PORT="${AUTH_PORT:-8551}"
P2P_PORT="${P2P_PORT:-30303}"
GAS_LIMIT="${GAS_LIMIT:-600000000}"
BLOCK_TIME="${BLOCK_TIME:-1s}"

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
DATADIR="${DATADIR:-${PROJECT_ROOT}/jit-data}"

# Strip OTEL env vars inherited from Claude Code: reth's --tracing-otlp-protocol
# only accepts `http`/`grpc`, but Claude Code sets OTEL_EXPORTER_OTLP_PROTOCOL=http/json.
unset OTEL_METRICS_EXPORTER OTEL_LOGS_EXPORTER OTEL_TRACES_EXPORTER \
      OTEL_EXPORTER_OTLP_PROTOCOL OTEL_EXPORTER_OTLP_ENDPOINT \
      OTEL_EXPORTER_OTLP_METRICS_ENDPOINT OTEL_EXPORTER_OTLP_LOGS_ENDPOINT \
      OTEL_EXPORTER_OTLP_TRACES_ENDPOINT OTEL_EXPORTER_OTLP_HEADERS \
      OTEL_EXPORTER_OTLP_METRICS_TEMPORALITY_PREFERENCE OTEL_SERVICE_NAME

# JIT toggle (default on). 0 = community-port runtime exists but disabled.
export XLAYER_JIT_ENABLED="${XLAYER_JIT_ENABLED:-1}"

BIN="${SCRIPT_DIR}/bin/xlayer-reth-jit"
GENESIS="${SCRIPT_DIR}/bench-data/bench-opstack-genesis-nojovian.json"

if [ "$XLAYER_JIT_ENABLED" = "1" ]; then
    echo ">>> Starting node with JIT runtime ENABLED (upstream paradigmxyz/revmc, no private patches)"
else
    echo ">>> Starting node with JIT runtime DISABLED (XLAYER_JIT_ENABLED=$XLAYER_JIT_ENABLED)"
fi

if [ ! -f "$BIN" ]; then
    echo "Error: binary not found at $BIN"
    echo "Build first: LLVM_SYS_220_PREFIX=/opt/homebrew/opt/llvm PATH=/opt/homebrew/opt/llvm/bin:\$PATH cargo build --release -p xlayer-reth-node --features jit"
    exit 1
fi

mkdir -p "$DATADIR"

# Reth dev-mode env knobs matching SA-Bench JIT comprehensive test (memory:
# 30_xlayer_sa_benchmark_jit_comprehensive_report).
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
    --http \
    --http.port "$PORT" \
    --http.api eth,debug,net,web3,txpool \
    --authrpc.port "$AUTH_PORT" \
    --discovery.port "$P2P_PORT" \
    --rpc.gascap 1500000000 \
    --ipcdisable \
    --log.stdout.filter "info,engine::tree::payload_validator=debug"
