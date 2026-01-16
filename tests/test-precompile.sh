#!/bin/bash

set -e

RPC_URL="http://localhost:8124"
POSEIDON_ADDRESS="0x0000000000000000000000000000000000000100"
INPUT="0x0000000000000000000000000000000000000000000000000000000000000001"
GAS_LIMIT="0x186a0"

cast rpc eth_call \
  "[{\"to\":\"$POSEIDON_ADDRESS\",\"data\":\"$INPUT\",\"gas\":\"$GAS_LIMIT\"},\"latest\"]" \
  --raw \
  --rpc-url "$RPC_URL" > /dev/null
echo "Call success"

GAS_USED=$(cast rpc eth_estimateGas \
  "[{\"to\":\"$POSEIDON_ADDRESS\",\"data\":\"$INPUT\",\"gas\":\"$GAS_LIMIT\"}]" \
  --raw \
  --rpc-url "$RPC_URL")
echo "Gas used: $GAS_USED"
