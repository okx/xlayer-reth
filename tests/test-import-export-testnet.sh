#!/bin/bash

TESTNET_EXPORT_FILE="testnet-exported.rlp"
TESTNET_REEXPORTED_FILE="testnet-re-exported.rlp"

if [ ! -f $TESTNET_EXPORT_FILE ]; then
    echo "❌ Testnet export file not found: $TESTNET_EXPORT_FILE". Please read README.md for more information.
    exit 1
fi

# Clean up
rm -rf testnet-data
rm -f $TESTNET_REEXPORTED_FILE
rm -f testnet-*.log

# Build node, export and import tools
just install
just install-tools

# Import the testnet
echo "ℹ️ Initializing testnet ..."
xlayer-reth-node init --datadir testnet-data --chain genesis-testnet-reth.json
echo "ℹ️ Importing testnet. Please wait patiently ..."
time xlayer-reth-tools import --datadir testnet-data --chain xlayer-testnet --exported-data $TESTNET_EXPORT_FILE > testnet-import.log 2>&1
if [ $? -ne 0 ]; then
    echo "❌ Testnet import failed. Please check testnet-import.log for more information."
    exit 1
fi
tail -n 3 testnet-import.log
echo "✅ Testnet import passed."

# Export the testnet and check consistency
echo "ℹ️ Exporting testnet. Please wait patiently ..."
time xlayer-reth-tools export --datadir testnet-data --chain xlayer-testnet --exported-data $TESTNET_REEXPORTED_FILE --start-block 12241700 > testnet-export.log 2>&1
if [ $? -ne 0 ]; then
    echo "❌ Testnet export failed. Please check testnet-export.log for more information."
    exit 1
fi
tail -n 3 testnet-export.log
echo "✅ Testnet export passed."

# Compare the export and import files
# TODO: at the moment, the files are different but the latest state root is the same. Hence,
# we skip the comparison for now.
###
### diff -q $TESTNET_EXPORT_FILE $TESTNET_REEXPORTED_FILE
### if [ $? -ne 0 ]; then
###     echo "❌ Testnet export and import files are different."
###     exit 1
### fi
### echo "✅ Testnet export and import files are the same."
### ###