#!/bin/bash

# Build the export and import tools
cd ..
just install-export
just install-import

# Clean up
cd tests
rm -rf data
rm -rf op-reth-seq
rm -f *.log
rm -f *.bin

# Extract the database
tar xf op-reth-seq.tar.xz

# Export the blocks
xlayer-reth-export --datadir op-reth-seq --chain genesis-reth.json --exported-data exp-test-78.bin --start-block 8593921 --end-block 8593999 | tee export-78.log
xlayer-reth-export --datadir op-reth-seq --chain genesis-reth.json --exported-data exp-test-all.bin | tee export-all.log
echo "*** Done exporting."

# Import the blocks
xlayer-reth-import --datadir data --chain genesis-reth.json --exported-data exp-test-78.bin | tee import-78.log
xlayer-reth-import --datadir data --chain genesis-reth.json --exported-data exp-test-all.bin | tee import-all.log
echo "*** Done importing."