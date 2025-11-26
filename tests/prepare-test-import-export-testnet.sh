#!/bin/bash

TESTNET_PATH=$1

if [ -z "$TESTNET_PATH" ]; then
    echo "❌ Testnet path not provided. Please run: ./prepare-test-import-export-testnet.sh <path-to-testnet-geth>"
    exit 1
fi

if [ ! -d "$TESTNET_PATH" ]; then
    echo "❌ Testnet path not found: $TESTNET_PATH"
    exit 1
fi

TESTNET_EXPORT_FILE="exported-testnet.rlp"
TESTNET_GENESIS_FILE="genesis-testnet-reth.json"

docker run --rm -v $TESTNET_PATH:/data xlayer/op-geth:v0.0.9 export --datadir=/data /data/$TESTNET_EXPORT_FILE
cp $TESTNET_PATH/$TESTNET_EXPORT_FILE .