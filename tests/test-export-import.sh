#!/bin/bash

# Clean up
rm -rf data
rm -rf op-reth-seq
rm -f devnet-*.log
rm -f devnet-*.rlp

echo "✅ Cleaned up successfully."
if [ "$1" = "clean" ]; then
    exit 0
fi

# Build the export and import tools
just install-tools

# Extract the database
echo "⚙️ Extracting blockchain data ..."
tar xf op-reth-seq.tar.xz

# Export the blocks
echo "⚙️ Exporting blocks ..."
xlayer-reth-tools export --datadir op-reth-seq --chain genesis-reth.json --exported-data devnet-exp-test-78.rlp --start-block 8593921 --end-block 8593999 | tee devnet-export-78.log
xlayer-reth-tools export --datadir op-reth-seq --chain genesis-reth.json --exported-data devnet-exp-test-all.rlp --start-block 8593921 | tee devnet-export-all.log
xlayer-reth-tools export --datadir op-reth-seq --chain genesis-reth.json --exported-data devnet-exp-test-all.rlp | tee devnet-export-err.log
if [ ${PIPESTATUS[0]} -eq 0 ]; then
    echo "❌ Export was supposed to fail, but didn't."
    exit 1
fi
echo "✅ Done exporting."

# Import the blocks
echo "⚙️ Importing blocks ..."
xlayer-reth-tools import --datadir data --chain genesis-reth.json --exported-data devnet-exp-test-78.rlp | tee devnet-import-78.log
xlayer-reth-tools import --datadir data --chain genesis-reth.json --exported-data devnet-exp-test-all.rlp | tee devnet-import-all.log
echo "✅ Done importing."

# Export and check
xlayer-reth-tools export --datadir data --chain genesis-reth.json --start-block 8593921 --exported-data devnet-exp-test-all-2.rlp
diff -q devnet-exp-test-all.rlp devnet-exp-test-all-2.rlp
if [ $? -ne 0 ]; then
    echo "❌ Export and check failed."
    exit 1
fi
echo "✅ Export and check passed."