# Module: xlayer-reth-tools

**Path**: `bin/tools/`
**Purpose**: Offline CLI utilities for data management and migration

## Commands

### `import` (`import.rs`)
- Imports RLP-encoded blocks from a file (supports gzip)
- Uses upstream `import_blocks_from_file` with OP executor and consensus
- Reports partial import on failure (blocks imported vs decoded)
- Flags: `--exported-data`, `--no-state`, `--chunk-len`

### `export` (`export.rs`)
- Exports blocks to RLP-encoded file (supports gzip compression via `.gz` extension)
- Validates: start/end block bounds, genesis block constraint
- Batch processing with configurable batch size (default: 100,000)
- Parallel RLP encoding via rayon
- Graceful Ctrl+C handling; removes incomplete output file on error
- Flags: `--exported-data`, `--start-block`, `--end-block`, `--batch-size`

### `gen-genesis` (`gen_genesis.rs`)
- Generates genesis file from an existing op-reth database
- Reads template genesis for `config` and header fields
- Iterates all accounts: balances, nonces, storage slots, bytecodes
- Template alloc entries take priority over database entries
- Sets `legacyXLayerBlock` and `number` to `latest_block + 1`
- Optional `--output-chainspec` for genesis without alloc field
- Flags: `--template-genesis`, `--output`, `--output-chainspec`, `--batch-size`

### `legacy-migrate` (`legacy_migrate/`)
- Migrates from MDBX to RocksDB + static files
- **Static file segments**: TransactionSenders, AccountChangeSets, StorageChangeSets, Receipts
- **RocksDB tables**: TransactionHashNumbers, AccountsHistory, StoragesHistory
- Parallel migration: static files and RocksDB migrate concurrently via `std::thread::scope`
- Within static files: segments migrate in parallel via rayon
- Receipt migration skipped if receipt pruning is active
- Finalization: writes v2 storage settings, optionally clears MDBX tables
- Progress logging every 10 seconds with ETA calculation
- Flags: `--batch-size`, `--skip-static-files`, `--skip-rocksdb`, `--keep-mdbx`

## Key Design Points

1. **Database safety**: `gen-genesis` warns to stop the node first — holds a long-lived read transaction that prevents MDBX page reclamation.
2. **Interrupt handling**: All long-running commands install Ctrl+C handlers for graceful shutdown.
3. **File cleanup**: Failed exports and genesis writes clean up partial output files.
4. **Migration atomicity**: v2 storage settings are only written when ALL migrations complete (including receipts). If receipts are skipped due to pruning, v1 settings are kept and MDBX tables are preserved.
5. **Gap handling**: Static file migration uses `ensure_at_block()` to fill gaps between blocks that have no changeset data.
