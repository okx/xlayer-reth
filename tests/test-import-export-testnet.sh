#!/bin/bash

TESTNET_EXPORT_FILE="exported-testnet.rlp"
TESTNET_REEXPORTED_FILE="re-exported-testnet.rlp"

if [ ! -f $TESTNET_EXPORT_FILE ]; then
    echo "❌ Testnet export file not found: $TESTNET_EXPORT_FILE". Please read README.md for more information.
    exit 1
fi

# Clean up
rm -rf testnet-data
rm -f $TESTNET_REEXPORTED_FILE

# Build the export and import tools
just install-export
just install-import

# Import the testnet
time xlayer-reth-import --datadir testnet-data --chain xlayer-testnet --exported-data $TESTNET_EXPORT_FILE > import-testnet.log 2>&1
if [ $? -ne 0 ]; then
    echo "❌ Testnet import failed. Please check import-testnet.log for more information."
    exit 1
fi
tail -n 3 import-testnet.log
echo "✅ Testnet import passed."

# Export the testnet and check consistency
time xlayer-reth-export --datadir testnet-data --chain xlayer-testnet --exported-data $TESTNET_REEXPORTED_FILE --start-block 12241700 > export-testnet.log 2>&1
if [ $? -ne 0 ]; then
    echo "❌ Testnet export failed. Please check export-testnet.log for more information."
    exit 1
fi
tail -n 3 export-testnet.log
echo "✅ Testnet export passed."

# Compare the export and import files
diff -q $TESTNET_EXPORT_FILE $TESTNET_REEXPORTED_FILE
if [ $? -ne 0 ]; then
    echo "❌ Testnet export and import files are different."
    exit 1
fi
echo "✅ Testnet export and import files are the same."