# XLayer Reth Export

A command-line tool for exporting blockchain data from XLayer Reth to RLP-encoded files, similar to go-ethereum's `export` command.

## Overview

The `xlayer-reth-export` tool allows you to export blockchain data from your XLayer Reth node's database to RLP-encoded block files. This is useful for:

- Creating backups of blockchain data
- Migrating data between nodes
- Sharing blockchain data for testing purposes
- Creating snapshots at specific block heights
- Fast-syncing other nodes using exported data

## Features

- **RLP Block Export**: Exports blocks to RLP-encoded format
- **Gzip Compression**: Automatically compresses output when using `.gz` extension
- **Range Selection**: Export specific block ranges (start/end blocks)
- **Batch Processing**: Efficiently reads blocks in configurable batches
- **Progress Reporting**: Shows real-time export progress
- **Graceful Interruption**: Handles Ctrl+C gracefully
- **Read-Only Access**: Only requires read access to the database

## Building

Build the export tool from the workspace root:

```bash
# Development build
cargo build --package xlayer-reth-export

# Optimized release build
cargo build --release --package xlayer-reth-export
```

The binary will be located at:
- Debug: `./target/debug/xlayer-reth-export`
- Release: `./target/release/xlayer-reth-export`

## Usage

### Basic Command

```bash
xlayer-reth-export --datadir <DATA_DIR> --chain <CHAIN_SPEC> --exported-data <OUTPUT_FILE>
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

## Examples

### Example 1: Export All Blocks

Export all blocks from genesis to the latest block:

```bash
xlayer-reth-export \
    --datadir /data/xlayer-reth \
    --chain optimism \
    --exported-data /backups/blocks.rlp
```

### Example 2: Export with Compression

Export all blocks to a compressed file:

```bash
xlayer-reth-export \
    --datadir /data/xlayer-reth \
    --chain optimism \
    --exported-data /backups/blocks.rlp.gz
```

### Example 3: Export Specific Block Range

Export blocks from 1000 to 5000:

```bash
xlayer-reth-export \
    --datadir /data/xlayer-reth \
    --chain optimism \
    --start-block 1000 \
    --end-block 5000 \
    --exported-data /backups/blocks-1000-5000.rlp.gz
```

### Example 4: Export Recent Blocks Only

Export the latest 10,000 blocks:

```bash
# First, get the latest block number
LATEST_BLOCK=$(curl -X POST -H "Content-Type: application/json" \
    --data '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}' \
    http://localhost:8545 | jq -r '.result' | xargs printf "%d\n")

# Calculate start block
START_BLOCK=$((LATEST_BLOCK - 10000))

# Export
xlayer-reth-export \
    --datadir /data/xlayer-reth \
    --chain optimism \
    --start-block $START_BLOCK \
    --exported-data /backups/recent-blocks.rlp.gz
```

### Example 5: Export with Custom Batch Size

Export with a larger batch size for better performance:

```bash
xlayer-reth-export \
    --datadir /data/xlayer-reth \
    --chain optimism \
    --batch-size 5000 \
    --exported-data /backups/blocks.rlp.gz
```

### Example 6: Export Using Custom Genesis

Export using a custom genesis file:

```bash
xlayer-reth-export \
    --datadir /data/xlayer-reth \
    --chain /path/to/custom-genesis.json \
    --exported-data /backups/blocks.rlp.gz
```

### Example 7: Export to Network Storage

Export directly to a network-mounted backup directory:

```bash
xlayer-reth-export \
    --datadir /data/xlayer-reth \
    --chain optimism \
    --exported-data /mnt/network-backup/xlayer-blocks-$(date +%Y%m%d).rlp.gz
```

## Importing Exported Data

Once you've exported blockchain data, you can import it into another node using the `xlayer-reth-import` command:

```bash
xlayer-reth-import \
    --datadir /data/new-node \
    --chain optimism \
    --exported-data /backups/blocks.rlp.gz
```

See the [xlayer-reth-import README](../import/README.md) for more details.

## Output

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

## Performance Considerations

### Export Speed

Export speed depends on several factors:
- **Disk I/O**: SSD is significantly faster than HDD
- **Batch Size**: Larger batch sizes can improve performance (but use more memory)
- **Compression**: Gzip compression adds CPU overhead but saves disk space
- **Database Size**: Larger databases may have slower reads

### Typical Performance

On modern hardware with SSD:
- **Uncompressed**: 500-1000 blocks/second
- **Compressed**: 300-500 blocks/second (depends on CPU)

### Optimization Tips

For faster exports:
- Use an SSD for the datadir
- Increase `--batch-size` (e.g., 5000 or 10000)
- Use uncompressed files during export, compress later if needed
- Ensure sufficient available RAM

## Troubleshooting

### Block Not Found Errors

If export fails with "Block X not found in database":
- Verify the block range exists in your database
- Check if the database is corrupted
- Ensure the node has fully synced to the requested block height

### Disk Space Issues

If you run out of disk space:
- Use gzip compression (`.gz` extension)
- Export in smaller ranges
- Clean up old export files before exporting
- Monitor available disk space with `df -h`

### Performance Issues

If export is slow:
- Increase `--batch-size` to 5000 or 10000
- Use an SSD for both datadir and output location
- Avoid network storage if possible (export locally, then move)
- Check system I/O with `iostat -x 1`

### Database Lock Issues

If you get database lock errors:
- Ensure no other processes are writing to the database
- Use a read replica if available
- Stop the node before exporting (if acceptable)

### Out of Memory Errors

If export crashes with OOM:
- Reduce `--batch-size` to 100 or 500
- Close other applications to free memory
- Check available memory with `free -h`

## Built-in Chain Specifications

The export tool supports the following built-in chains via the `--chain` flag:

- `optimism` - Optimism Mainnet
- `optimism-sepolia`, `optimism_sepolia` - Optimism Sepolia Testnet
- `base` - Base Mainnet
- `base-sepolia`, `base_sepolia` - Base Sepolia Testnet
- And many other OP Stack chains (see `--help` for full list)

## File Format

The exported file contains RLP-encoded blocks in sequence:

```
[Block 0 RLP][Block 1 RLP][Block 2 RLP]...[Block N RLP]
```

Each block is encoded according to the Ethereum RLP specification, including:
- Block header
- List of transactions
- List of uncle headers (if any)

## Use Cases

### 1. Node Migration

Export from old node, import to new node:

```bash
# On old node
xlayer-reth-export \
    --datadir /data/old-node \
    --chain optimism \
    --exported-data /transfer/blocks.rlp.gz

# On new node
xlayer-reth-import \
    --datadir /data/new-node \
    --chain optimism \
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

xlayer-reth-export \
    --datadir /data/xlayer-reth \
    --chain optimism \
    --start-block $START_BLOCK \
    --end-block $END_BLOCK \
    --exported-data /backups/blocks-$TODAY.rlp.gz
```

### 3. Testing Environments

Export production data for testing:

```bash
# Export recent 1000 blocks from production
xlayer-reth-export \
    --datadir /data/prod-node \
    --chain optimism \
    --start-block 1000000 \
    --end-block 1001000 \
    --exported-data /test-data/recent-blocks.rlp

# Import to test node
xlayer-reth-import \
    --datadir /data/test-node \
    --chain optimism \
    --exported-data /test-data/recent-blocks.rlp
```

### 4. Data Analysis

Export specific block ranges for analysis:

```bash
xlayer-reth-export \
    --datadir /data/xlayer-reth \
    --chain optimism \
    --start-block 500000 \
    --end-block 500100 \
    --exported-data /analysis/sample-blocks.rlp
```

## Best Practices

1. **Always Use Compression**: Add `.gz` extension to save ~70% disk space
2. **Monitor Disk Space**: Ensure 2-3x free space of expected export size
3. **Test Small Ranges First**: Test with a small block range before exporting large datasets
4. **Use Absolute Paths**: Always use absolute paths for reliability
5. **Verify After Export**: Check file size and optionally import to verify
6. **Schedule During Off-Peak**: Run large exports during low-traffic periods
7. **Keep Multiple Backups**: Maintain multiple backup copies in different locations

## Security Considerations

- **Read-Only Operation**: Export only reads from the database, no writes
- **No Network Access**: Export doesn't require network connectivity
- **File Permissions**: Ensure exported files have appropriate permissions
- **Sensitive Data**: Exported files contain full blockchain data (transactions visible)

## Support

For issues or questions:
- Check the [XLayer Reth repository](https://github.com/okx/xlayer-reth)
- Review the main [Reth documentation](https://github.com/paradigmxyz/reth)
- Open an issue on GitHub

## License

This tool is part of XLayer Reth and is licensed under the same license as the main project.

