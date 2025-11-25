## Tests

### Export and Import Test

To run the export/import test, you first need to run ``op-reth`` to produce at least 1000 blocks. Please run [X Layer devnet](https://github.com/okx/xlayer-toolkit/tree/main/devnet) for at least 1000 blocks and save ``genesis-reth.json`` file (found under ``devnet/config-op/``) and ``op-reth-seq`` folder (found under ``data/``). You can compress ``op-reth-seq`` folder as such:
```
tar cjf op-reth-seq.tar.xz op-reth-seq
```

Copy ``genesis-reth.json`` and ``op-reth-seq.tar.xz`` in this repo's ``tests`` folder and run the test:
```
./test-export-import.sh
```
