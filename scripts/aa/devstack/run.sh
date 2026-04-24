#!/usr/bin/env bash
# Hold-alive XLayer AA devnet driver.
#
# Starts L1 (in-process geth) + L2 (sequencer + validator, both xlayer-reth)
# via op-devstack sysgo. The test blocks on SIGINT so you can curl/cast
# against the live RPC endpoints that it prints on startup.
#
# Prereqs (installed via `mise install` in deps/optimism):
#   - go 1.24.x
#   - geth 1.16.x
#   - forge 1.2.3
#
# Before running, build the xlayer-reth-node binary and the op-stack
# contracts once:
#   just build              (or: cargo build -p xlayer-reth-node)
#   (cd deps/optimism/packages/contracts-bedrock && forge build --skip '/**/test/**')
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../.." && pwd)"
MISE_INSTALLS="$HOME/.local/share/mise/installs"

# Tool paths (adjust if your mise layout differs)
GO_BIN="$MISE_INSTALLS/go/1.24.10/bin"
GETH_BIN="$MISE_INSTALLS/go-github-com-ethereum-go-ethereum-cmd-geth/1.16.4/bin"
FORGE_BIN="$MISE_INSTALLS/forge/1.2.3"
ANVIL_BIN="$MISE_INSTALLS/anvil/1.2.3"
CAST_BIN="$MISE_INSTALLS/cast/1.2.3"

export PATH="$GO_BIN:$FORGE_BIN:$ANVIL_BIN:$CAST_BIN:$GETH_BIN:$PATH"
export SYSGO_GETH_EXEC_PATH="$GETH_BIN/geth"
export RUST_BINARY_PATH_OP_RETH="${RUST_BINARY_PATH_OP_RETH:-$REPO_ROOT/target/debug/xlayer-reth-node}"
export DISABLE_OP_E2E_LEGACY=true

# Both L2 ELs default to op-reth (xlayer-reth). Override for comparison runs.
export OP_DEVSTACK_PROOF_SEQUENCER_EL="${OP_DEVSTACK_PROOF_SEQUENCER_EL:-op-reth}"
export OP_DEVSTACK_PROOF_VALIDATOR_EL="${OP_DEVSTACK_PROOF_VALIDATOR_EL:-op-reth}"

if [[ ! -x "$RUST_BINARY_PATH_OP_RETH" ]]; then
	echo "ERROR: xlayer-reth-node binary not found at $RUST_BINARY_PATH_OP_RETH" >&2
	echo "       Build it first with: cargo build -p xlayer-reth-node" >&2
	exit 1
fi
if [[ ! -x "$SYSGO_GETH_EXEC_PATH" ]]; then
	echo "ERROR: geth binary not found at $SYSGO_GETH_EXEC_PATH" >&2
	echo "       Install it via: (cd $REPO_ROOT/deps/optimism && mise install)" >&2
	exit 1
fi

cd "$SCRIPT_DIR"

if [[ ! -f go.sum ]]; then
	echo "==> First run — resolving Go modules (large download, a few minutes)"
	go mod tidy
fi

LOG_FILE="${LOG_FILE:-$SCRIPT_DIR/devnet.log}"
echo "==> Full logs: $LOG_FILE"

# -v streams output immediately (required for fmt.Printf banner to appear — without
# -v the test runner buffers and discards all stdout on success).
# logfilter in holdalive_test.go already drops INFO; only WARN/ERROR + the banner show.
# Full output saved to LOG_FILE for post-mortem.
go test -v -count=1 -timeout=0 -run TestXLayerAAHoldAlive . 2>&1 | tee "$LOG_FILE"
