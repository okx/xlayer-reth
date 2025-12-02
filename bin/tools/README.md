# XLayer Reth Tools

A unified command-line tool for importing and exporting blockchain data with XLayer Reth, similar to go-ethereum's `import` and `export` commands.

## Overview

The `xlayer-reth-tools` provides two main utilities:

- **Import**: Import blockchain data from RLP-encoded block files into your XLayer Reth node
- **Export**: Export blockchain data from your XLayer Reth node to RLP-encoded files

These tools are useful for:

- Fast-syncing a node from exported blockchain data
- Migrating data between nodes
- Testing with pre-generated blockchain data
- Bootstrapping a new node with historical data
- Creating backups of blockchain data
- Sharing blockchain data for testing purposes

## Features

### Import Features
- **RLP Block Import**: Imports RLP-encoded blocks from files
- **Gzip Support**: Automatically handles gzip-compressed files (`.gz`)
- **Batch Processing**: Efficiently imports blocks in configurable batches
- **Smart Skip**: Automatically skips genesis block and already-imported blocks
- **State Management**: Optional state processing with `--no-state` flag
- **Graceful Interruption**: Handles Ctrl+C gracefully

### Export Features
- **RLP Block Export**: Exports blocks to RLP-encoded format
- **Gzip Compression**: Automatically compresses output when using `.gz` extension
- **Range Selection**: Export specific block ranges (start/end blocks)
- **Batch Processing**: Efficiently reads blocks in configurable batches
- **Progress Reporting**: Shows real-time export progress
- **Read-Only Access**: Only requires read access to the database

## Building

Build the tool from the workspace root:

```bash
just build-tools
just install-tools
```

The binary will be located at:
- Debug: `./target/debug/xlayer-reth-tools`
- Release: `./target/release/xlayer-reth-tools`

---

## Import Command

The import command allows you to import blockchain data from RLP-encoded block files into your XLayer Reth node.

### Basic Command

```bash
xlayer-reth-tools import --datadir <DATA_DIR> --chain <CHAIN_SPEC> --exported-data <BLOCK_FILE>
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

### Import Examples

#### Example 1: Import from Local File

Import blocks from a local RLP file using the xlayer-testnet chain specification:

```bash
xlayer-reth-tools import \
    --datadir /data/xlayer-reth \
    --chain xlayer-testnet \
    --exported-data /path/to/blocks.rlp
```

#### Example 2: Import from Compressed File

Import from a gzip-compressed file:

```bash
xlayer-reth-tools import \
    --datadir /data/xlayer-reth \
    --chain xlayer-testnet \
    --exported-data /path/to/blocks.rlp.gz
```

#### Example 3: Import with Custom Genesis

Import using a custom genesis file:

```bash
xlayer-reth-tools import \
    --datadir /data/xlayer-reth \
    --chain /path/to/custom-genesis.json \
    --exported-data /path/to/blocks.rlp
```

#### Example 4: Fast Import (No State Processing)

Import without state processing for faster imports (useful for testing):

```bash
xlayer-reth-tools import \
    --datadir /data/xlayer-reth \
    --chain xlayer-testnet \
    --no-state \
    --exported-data /path/to/blocks.rlp
```

#### Example 5: Import with Custom Chunk Size

Import with a custom read chunk size:

```bash
xlayer-reth-tools import \
    --datadir /data/xlayer-reth \
    --chain xlayer-testnet \
    --chunk-len 1048576 \
    --exported-data /path/to/blocks.rlp
```

#### Example 6: Import with Database Configuration

Import with custom database settings:

```bash
xlayer-reth-tools import \
    --datadir /data/xlayer-reth \
    --chain xlayer-testnet \
    --db.max-size 2TB \
    --db.growth-step 4GB \
    --db.log-level notice \
    --exported-data /path/to/blocks.rlp
```

### Creating Exportable Data

To create an RLP-encoded blocks file that can be imported, you can use `geth` or the export command:

#### Using Geth

```bash
# Export blocks from geth
geth export /path/to/output.rlp <start_block> <end_block>

# Export and compress
geth export /path/to/output.rlp <start_block> <end_block>
gzip /path/to/output.rlp
```

#### Using XLayer Reth Tools

```bash
# Export using xlayer-reth-tools export command (see Export Command section)
xlayer-reth-tools export --datadir <datadir> --exported-data /path/to/output.rlp
```

### Import Output

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

### Import Troubleshooting

#### Import Fails with "Chain was partially imported"

This indicates that not all blocks or transactions were successfully imported. Check:
- The RLP file is not corrupted
- The chain specification matches the exported data
- Sufficient disk space is available
- Database isn't corrupted

#### Database Size Issues

If you encounter database size errors:
- Increase `--db.max-size` (e.g., `--db.max-size 4TB`)
- Ensure sufficient disk space is available
- Consider using a larger growth step with `--db.growth-step`

#### Performance Optimization

For faster imports:
- Use `--no-state` to skip state processing (less validation)
- Use an SSD for the datadir
- Increase chunk size with `--chunk-len`
- Use a compressed (`.gz`) file to reduce I/O

#### File Format Errors

If the tool fails to read the file:
- Verify the file is RLP-encoded blocks format
- Check if the file is corrupted
- Ensure gzip files have `.gz` extension
- Try exporting the data again

### Import Technical Details

#### Block Import Process

1. **Read**: Reads RLP-encoded blocks from file in chunks
2. **Decode**: Decodes each block from RLP format
3. **Validate**: Validates block headers and consensus rules
4. **Execute**: Executes transactions (unless `--no-state` is used)
5. **Store**: Writes blocks and state to database
6. **Skip Duplicates**: Automatically skips already-imported blocks

#### Database Structure

The import tool uses the same database structure as the main XLayer Reth node:
- **MDBX Database**: For hot data (recent blocks, state)
- **Static Files**: For cold data (historical blocks)

---

## Export Command

The export command allows you to export blockchain data from your XLayer Reth node's database to RLP-encoded block files.

### Basic Command

```bash
xlayer-reth-tools export --datadir <DATA_DIR> --chain <CHAIN_SPEC> --exported-data <OUTPUT_FILE>
```

### Required Arguments

- `--exported-data <OUTPUT_FILE>`: Path to write the exported blocks (automatically compresses if ends with `.gz`)

### Important Options

- `--datadir <DIR>`: Directory containing the reth database (default: OS-specific)
- `--chain <CHAIN_SPEC>`: Chain specification - either a built-in chain name or path to genesis JSON file

### Optional Parameters

- `--start-block <NUM>`: Starting block number (inclusive, default: 0)
- `--end-block <NUM>`: Ending block number (inclusive, default: latest block)
- `--batch-size <NUM>`: Batch size for reading blocks (default: 1000)
- `--config <FILE>`: Path to a configuration file

### Database Options

- `--db.log-level <LEVEL>`: Database logging level
- `--db.max-size <SIZE>`: Maximum database size
- `--db.max-readers <NUM>`: Maximum number of concurrent readers

### Export Examples

#### Example 1: Export All Blocks

Export all blocks from genesis to the latest block:

```bash
xlayer-reth-tools export \
    --datadir /data/xlayer-reth \
    --chain xlayer-testnet \
    --exported-data /backups/blocks.rlp
```

#### Example 2: Export with Compression

Export all blocks to a compressed file:

```bash
xlayer-reth-tools export \
    --datadir /data/xlayer-reth \
    --chain xlayer-testnet \
    --exported-data /backups/blocks.rlp.gz
```

#### Example 3: Export Specific Block Range

Export blocks from 1000 to 5000:

```bash
xlayer-reth-tools export \
    --datadir /data/xlayer-reth \
    --chain xlayer-testnet \
    --start-block 1000 \
    --end-block 5000 \
    --exported-data /backups/blocks-1000-5000.rlp.gz
```

#### Example 4: Export Recent Blocks Only

Export the latest 10,000 blocks:

```bash
# First, get the latest block number
LATEST_BLOCK=$(curl -X POST -H "Content-Type: application/json" \
    --data '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}' \
    http://localhost:8545 | jq -r '.result' | xargs printf "%d\n")

# Calculate start block
START_BLOCK=$((LATEST_BLOCK - 10000))

# Export
xlayer-reth-tools export \
    --datadir /data/xlayer-reth \
    --chain xlayer-testnet \
    --start-block $START_BLOCK \
    --exported-data /backups/recent-blocks.rlp.gz
```

#### Example 5: Export with Custom Batch Size

Export with a larger batch size for better performance:

```bash
xlayer-reth-tools export \
    --datadir /data/xlayer-reth \
    --chain xlayer-testnet \
    --batch-size 5000 \
    --exported-data /backups/blocks.rlp.gz
```

#### Example 6: Export Using Custom Genesis

Export using a custom genesis file:

```bash
xlayer-reth-tools export \
    --datadir /data/xlayer-reth \
    --chain /path/to/custom-genesis.json \
    --exported-data /backups/blocks.rlp.gz
```

#### Example 7: Export to Network Storage

Export directly to a network-mounted backup directory:

```bash
xlayer-reth-tools export \
    --datadir /data/xlayer-reth \
    --chain xlayer-testnet \
    --exported-data /mnt/network-backup/xlayer-blocks-$(date +%Y%m%d).rlp.gz
```

### Importing Exported Data

Once you've exported blockchain data, you can import it into another node using the import command:

```bash
xlayer-reth-tools import \
    --datadir /data/new-node \
    --chain xlayer-testnet \
    --exported-data /backups/blocks.rlp.gz
```

See the [Import Command](#import-command) section for more details.

### Export Output

During export, the tool displays:

- Export progress with percentage complete
- Current block number being exported
- Periodic progress updates (every 1000 blocks)

Example output:

```
INFO xlayer::export: XLayer Reth Export starting
INFO reth::cli: reth v1.9.2 starting
INFO reth::cli: Exporting blockchain to file: /backups/blocks.rlp.gz
INFO reth::cli: Exporting blocks 0 to 10000 (10001 blocks total)
INFO reth::cli: Using gzip compression
INFO reth::cli: Exported 1000 blocks (10.00%) - Latest: #1000
INFO reth::cli: Exported 2000 blocks (20.00%) - Latest: #2000
...
INFO reth::cli: Export complete! Exported 10001 blocks to /backups/blocks.rlp.gz
```

### Export Performance Considerations

#### Export Speed

Export speed depends on several factors:
- **Disk I/O**: SSD is significantly faster than HDD
- **Batch Size**: Larger batch sizes can improve performance (but use more memory)
- **Compression**: Gzip compression adds CPU overhead but saves disk space
- **Database Size**: Larger databases may have slower reads

#### Typical Performance

On modern hardware with SSD:
- **Uncompressed**: 500-1000 blocks/second
- **Compressed**: 300-500 blocks/second (depends on CPU)

#### Optimization Tips

For faster exports:
- Use an SSD for the datadir
- Increase `--batch-size` (e.g., 5000 or 10000)
- Use uncompressed files during export, compress later if needed
- Ensure sufficient available RAM

### Export Troubleshooting

#### Block Not Found Errors

If export fails with "Block X not found in database":
- Verify the block range exists in your database
- Check if the database is corrupted
- Ensure the node has fully synced to the requested block height

#### Disk Space Issues

If you run out of disk space:
- Use gzip compression (`.gz` extension)
- Export in smaller ranges
- Clean up old export files before exporting
- Monitor available disk space with `df -h`

#### Performance Issues

If export is slow:
- Increase `--batch-size` to 5000 or 10000
- Use an SSD for both datadir and output location
- Avoid network storage if possible (export locally, then move)
- Check system I/O with `iostat -x 1`

#### Database Lock Issues

If you get database lock errors:
- Ensure no other processes are writing to the database
- Use a read replica if available
- Stop the node before exporting (if acceptable)

#### Out of Memory Errors

If export crashes with OOM:
- Reduce `--batch-size` to 100 or 500
- Close other applications to free memory
- Check available memory with `free -h`

### Export File Format

The exported file contains RLP-encoded blocks in sequence:

```
[Block 0 RLP][Block 1 RLP][Block 2 RLP]...[Block N RLP]
```

Each block is encoded according to the Ethereum RLP specification, including:
- Block header
- List of transactions
- List of uncle headers (if any)

---

## Use Cases

### 1. Node Migration

Export from old node, import to new node:

```bash
# On old node
xlayer-reth-tools export \
    --datadir /data/old-node \
    --chain xlayer-testnet \
    --exported-data /transfer/blocks.rlp.gz

# On new node
xlayer-reth-tools import \
    --datadir /data/new-node \
    --chain xlayer-testnet \
    --exported-data /transfer/blocks.rlp.gz
```

### 2. Incremental Backups

Create daily incremental backups:

```bash
#!/bin/bash
TODAY=$(date +%Y%m%d)
YESTERDAY=$(date -d "yesterday" +%Y%m%d)

# Get block numbers for yesterday and today
START_BLOCK=$(get_block_at_date $YESTERDAY)
END_BLOCK=$(get_block_at_date $TODAY)

xlayer-reth-tools export \
    --datadir /data/xlayer-reth \
    --chain xlayer-testnet \
    --start-block $START_BLOCK \
    --end-block $END_BLOCK \
    --exported-data /backups/blocks-$TODAY.rlp.gz
```

### 3. Testing Environments

Export production data for testing:

```bash
# Export recent 1000 blocks from production
xlayer-reth-tools export \
    --datadir /data/prod-node \
    --chain xlayer-testnet \
    --start-block 1000000 \
    --end-block 1001000 \
    --exported-data /test-data/recent-blocks.rlp

# Import to test node
xlayer-reth-tools import \
    --datadir /data/test-node \
    --chain xlayer-testnet \
    --exported-data /test-data/recent-blocks.rlp
```

### 4. Data Analysis

Export specific block ranges for analysis:

```bash
xlayer-reth-tools export \
    --datadir /data/xlayer-reth \
    --chain xlayer-testnet \
    --start-block 500000 \
    --end-block 500100 \
    --exported-data /analysis/sample-blocks.rlp
```

---

## Best Practices

### Import Best Practices

1. **Backup First**: Always backup your datadir before importing
2. **Use Matching Chain**: Ensure chain spec matches the exported data
3. **Monitor Disk Space**: Ensure sufficient space (2-3x the export file size)
4. **Compressed Files**: Use `.gz` files to save disk space and reduce I/O
5. **Test First**: Test with `--no-state` on a small subset before full import

### Export Best Practices

1. **Always Use Compression**: Add `.gz` extension to save ~70% disk space
2. **Monitor Disk Space**: Ensure 2-3x free space of expected export size
3. **Test Small Ranges First**: Test with a small block range before exporting large datasets
4. **Use Absolute Paths**: Always use absolute paths for reliability
5. **Verify After Export**: Check file size and optionally import to verify
6. **Schedule During Off-Peak**: Run large exports during low-traffic periods
7. **Keep Multiple Backups**: Maintain multiple backup copies in different locations

---

## Security Considerations

- **Read-Only Export**: Export only reads from the database, no writes
- **Import Validation**: Import validates blocks and transactions before storing
- **No Network Access**: Neither command requires network connectivity
- **File Permissions**: Ensure exported files have appropriate permissions
- **Sensitive Data**: Exported files contain full blockchain data (transactions visible)

---

## Support

For issues or questions:
- Check the [XLayer Reth repository](https://github.com/okx/xlayer-reth)
- Review the main [Reth documentation](https://github.com/paradigmxyz/reth)
- Open an issue on GitHub

## License

This tool is part of XLayer Reth and is licensed under the same license as the main project.