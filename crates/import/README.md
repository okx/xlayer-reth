# XLayer Reth Import

A command-line tool for importing RLP-encoded blockchain data into XLayer Reth, similar to go-ethereum's `import` command.

## Overview

The `xlayer-reth-import` tool allows you to import blockchain data from RLP-encoded block files into your XLayer Reth node. This is useful for:

- Fast-syncing a node from exported blockchain data
- Migrating data between nodes
- Testing with pre-generated blockchain data
- Bootstrapping a new node with historical data

## Features

- **RLP Block Import**: Imports RLP-encoded blocks from files
- **Gzip Support**: Automatically handles gzip-compressed files (`.gz`)
- **Batch Processing**: Efficiently imports blocks in configurable batches
- **Smart Skip**: Automatically skips genesis block and already-imported blocks
- **State Management**: Optional state processing with `--no-state` flag
- **Graceful Interruption**: Handles Ctrl+C gracefully

## Building

Build the import tool from the workspace root:

```bash
# Development build
cargo build --package xlayer-reth-import

# Optimized release build
cargo build --release --package xlayer-reth-import
```

The binary will be located at:
- Debug: `./target/debug/xlayer-reth-import`
- Release: `./target/release/xlayer-reth-import`

## Usage

### Basic Command

```bash
xlayer-reth-import --datadir <DATA_DIR> --chain <CHAIN_SPEC> --exported-data <BLOCK_FILE>
```

### Required Arguments

- `--exported-data <BLOCK_FILE>`: Path to the RLP-encoded blocks file (supports `.gz` compression)

### Important Options

- `--datadir <DIR>`: Directory for all reth files and subdirectories (default: OS-specific)
- `--chain <CHAIN_SPEC>`: Chain specification - either a built-in chain name or path to genesis JSON file

### Optional Flags

- `--no-state`: Disables stages that require state processing (faster but less validation)
- `--chunk-len <SIZE>`: Chunk byte length to read from file
- `--config <FILE>`: Path to a configuration file

### Database Options

- `--db.log-level <LEVEL>`: Database logging level (fatal, error, warn, notice, verbose, debug, trace, extra)
- `--db.max-size <SIZE>`: Maximum database size (e.g., 4TB, 8MB)
- `--db.growth-step <SIZE>`: Database growth step (e.g., 4GB, 4KB)
- `--db.max-readers <NUM>`: Maximum number of concurrent readers

## Examples

### Example 1: Import from Local File

Import blocks from a local RLP file using the Optimism chain specification:

```bash
xlayer-reth-import \
    --datadir /data/xlayer-reth \
    --chain optimism \
    --exported-data /path/to/blocks.rlp
```

### Example 2: Import from Compressed File

Import from a gzip-compressed file:

```bash
xlayer-reth-import \
    --datadir /data/xlayer-reth \
    --chain optimism \
    --exported-data /path/to/blocks.rlp.gz
```

### Example 3: Import with Custom Genesis

Import using a custom genesis file:

```bash
xlayer-reth-import \
    --datadir /data/xlayer-reth \
    --chain /path/to/custom-genesis.json \
    --exported-data /path/to/blocks.rlp
```

### Example 4: Fast Import (No State Processing)

Import without state processing for faster imports (useful for testing):

```bash
xlayer-reth-import \
    --datadir /data/xlayer-reth \
    --chain optimism \
    --no-state \
    --exported-data /path/to/blocks.rlp
```

### Example 5: Import with Custom Chunk Size

Import with a custom read chunk size:

```bash
xlayer-reth-import \
    --datadir /data/xlayer-reth \
    --chain optimism \
    --chunk-len 1048576 \
    --exported-data /path/to/blocks.rlp
```

### Example 6: Import with Database Configuration

Import with custom database settings:

```bash
xlayer-reth-import \
    --datadir /data/xlayer-reth \
    --chain optimism \
    --db.max-size 2TB \
    --db.growth-step 4GB \
    --db.log-level notice \
    --exported-data /path/to/blocks.rlp
```

## Exporting Blockchain Data

To create an RLP-encoded blocks file that can be imported, you can use `geth` or another compatible tool:

### Using Geth

```bash
# Export blocks from geth
geth export /path/to/output.rlp <start_block> <end_block>

# Export and compress
geth export /path/to/output.rlp <start_block> <end_block>
gzip /path/to/output.rlp
```

### Using Reth

```bash
# Export using reth (if available in your version)
reth export --datadir <datadir> --out /path/to/output.rlp
```

## Built-in Chain Specifications

The import tool supports the following built-in chains via the `--chain` flag:

- `optimism` - Optimism Mainnet
- `optimism-sepolia`, `optimism_sepolia` - Optimism Sepolia Testnet
- `base` - Base Mainnet
- `base-sepolia`, `base_sepolia` - Base Sepolia Testnet
- And many other OP Stack chains (see `--help` for full list)

## Output

During import, the tool will display:

- Import progress and block numbers
- Total blocks and transactions imported
- Any errors or warnings encountered

Example output:

```
INFO xlayer::import: XLayer Reth Import starting
INFO reth::cli: reth v1.9.2 starting
INFO reth::cli: Importing blockchain from file: /path/to/blocks.rlp
INFO reth::cli: Import complete! Imported 1000/1000 blocks, 50000/50000 transactions
```

## Troubleshooting

### Import Fails with "Chain was partially imported"

This indicates that not all blocks or transactions were successfully imported. Check:
- The RLP file is not corrupted
- The chain specification matches the exported data
- Sufficient disk space is available
- Database isn't corrupted

### Database Size Issues

If you encounter database size errors:
- Increase `--db.max-size` (e.g., `--db.max-size 4TB`)
- Ensure sufficient disk space is available
- Consider using a larger growth step with `--db.growth-step`

### Performance Optimization

For faster imports:
- Use `--no-state` to skip state processing (less validation)
- Use an SSD for the datadir
- Increase chunk size with `--chunk-len`
- Use a compressed (`.gz`) file to reduce I/O

### File Format Errors

If the tool fails to read the file:
- Verify the file is RLP-encoded blocks format
- Check if the file is corrupted
- Ensure gzip files have `.gz` extension
- Try exporting the data again

## Technical Details

### Block Import Process

1. **Read**: Reads RLP-encoded blocks from file in chunks
2. **Decode**: Decodes each block from RLP format
3. **Validate**: Validates block headers and consensus rules
4. **Execute**: Executes transactions (unless `--no-state` is used)
5. **Store**: Writes blocks and state to database
6. **Skip Duplicates**: Automatically skips already-imported blocks

### Database Structure

The import tool uses the same database structure as the main XLayer Reth node:
- **MDBX Database**: For hot data (recent blocks, state)
- **Static Files**: For cold data (historical blocks)

## Best Practices

1. **Backup First**: Always backup your datadir before importing
2. **Use Matching Chain**: Ensure chain spec matches the exported data
3. **Monitor Disk Space**: Ensure sufficient space (2-3x the export file size)
4. **Compressed Files**: Use `.gz` files to save disk space and reduce I/O
5. **Test First**: Test with `--no-state` on a small subset before full import

## Support

For issues or questions:
- Check the [XLayer Reth repository](https://github.com/okx/xlayer-reth)
- Review the main [Reth documentation](https://github.com/paradigmxyz/reth)
- Open an issue on GitHub

## License

This tool is part of XLayer Reth and is licensed under the same license as the main project.

