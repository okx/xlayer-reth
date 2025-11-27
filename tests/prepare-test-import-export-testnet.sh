#!/bin/bash

sed_inplace() {
  if [[ "$OSTYPE" == "darwin"* ]]; then
    sed -i '' "$@"
  else
    sed -i "$@"
  fi
}

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
TESTNET_GENESIS_URL="https://okg-pub-hk.oss-cn-hongkong.aliyuncs.com/cdn/chain/xlayer/snapshot/merged.genesis.json.tar.gz"
TESTNET_NEXT_BLOCK_NUMBER=12241700

if [ ! -f "$TESTNET_GENESIS_FILE" ]; then
    echo "ℹ️ Downloading testnet genesis file ..."
    wget $TESTNET_GENESIS_URL
    tar -xzf merged.genesis.json.tar.gz
    mv merged.genesis.json $TESTNET_GENESIS_FILE
    sed_inplace 's/"number": "0x0"/"number": '$TESTNET_NEXT_BLOCK_NUMBER'/' $TESTNET_GENESIS_FILE
else
    echo "ℹ️ Testnet genesis file already exists: $TESTNET_GENESIS_FILE"
fi

if [ ! -f "$TESTNET_EXPORT_FILE" ]; then
    echo "ℹ️ Exporting testnet data ..."
    docker run --rm -v $TESTNET_PATH:/data xlayer/op-geth:v0.0.9 export --datadir=/data /data/$TESTNET_EXPORT_FILE
    cp $TESTNET_PATH/$TESTNET_EXPORT_FILE .
else
    echo "ℹ️ Testnet export file already exists: $TESTNET_EXPORT_FILE"
fi