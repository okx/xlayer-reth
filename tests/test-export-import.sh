#!/bin/bash

# Build the export and import tools
just install-export
just install-import

# Clean up
rm -rf data
rm -rf op-reth-seq
rm -f *.log
rm -f *.rlp

# Extract the database
echo "⚙️ Extracting blockchain data ..."
tar xf op-reth-seq.tar.xz

# Export the blocks
echo "⚙️ Exporting blocks ..."
xlayer-reth-export --datadir op-reth-seq --chain genesis-reth.json --exported-data exp-test-78.rlp --start-block 8593921 --end-block 8593999 | tee export-78.log
xlayer-reth-export --datadir op-reth-seq --chain genesis-reth.json --exported-data exp-test-all.rlp --start-block 8593921 | tee export-all.log
xlayer-reth-export --datadir op-reth-seq --chain genesis-reth.json --exported-data exp-test-all.rlp | tee export-err.log
if [ ${PIPESTATUS[0]} -eq 0 ]; then
    echo "❌ Export was supposed to fail, but didn't."
    exit 1
fi
echo "✅ Done exporting."

# Import the blocks
echo "⚙️ Importing blocks ..."
xlayer-reth-import --datadir data --chain genesis-reth.json --exported-data exp-test-78.rlp | tee import-78.log
xlayer-reth-import --datadir data --chain genesis-reth.json --exported-data exp-test-all.rlp | tee import-all.log
echo "✅ Done importing."

# Export and check
xlayer-reth-export --datadir data --chain genesis-reth.json --start-block 8593921 --exported-data exp-test-all-2.rlp
diff -q exp-test-all.rlp exp-test-all-2.rlp
if [ $? -ne 0 ]; then
    echo "❌ Export and check failed."
    exit 1
fi
echo "✅ Export and check passed."