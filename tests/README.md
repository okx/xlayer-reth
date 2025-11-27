## Tests

### Export and Import Test Using Devnet

To run the export/import test, you first need to run ``op-reth`` to produce at least 1000 blocks. Please run [X Layer devnet](https://github.com/okx/xlayer-toolkit/tree/main/devnet) for at least 1000 blocks and save ``genesis-reth.json`` file (found under ``devnet/config-op/``) and ``op-reth-seq`` folder (found under ``data/``). You can compress ``op-reth-seq`` folder as such:
```
tar cjf op-reth-seq.tar.xz op-reth-seq
```

Copy ``genesis-reth.json`` and ``op-reth-seq.tar.xz`` in this repo's ``tests`` folder and run the test:
```
./test-export-import.sh
```

### Import and Export Test Using Testnet

To run this test, you first need to sync an X Layer Testnet RPC following [this guide](https://github.com/okx/xlayer-toolkit/blob/main/rpc-setup/README.md). This will create a ``testnet-geth`` folder under your working directory. Set a variable ``TESTNET_PATH=<path-to-testnet-geth-inclusive>/op-geth/data``. Then, run:
```
./prepare-test-import-export-testnet.sh $TESTNET_PATH
```
This script will use op-geth to export testnet data in RLP format. Next, run the test:
```
./test-import-export-testnet.sh
```