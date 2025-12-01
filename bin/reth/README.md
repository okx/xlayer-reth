# XLayer Reth

A comprehensive blockchain client for the XLayer network, an Optimism-based Layer 2 solution. This tool provides a complete set of commands for running nodes, importing and exporting blockchain data.

## Overview

XLayer Reth is built on top of the [Reth](https://github.com/paradigmxyz/reth) execution client with custom features and optimizations for the XLayer network. The client supports:

- **Running XLayer Nodes**: Full node operation with custom XLayer features
- **Data Import**: Import blockchain data from RLP-encoded block files
- **Data Export**: Export blockchain data to RLP-encoded format
- **Chain Specifications**: Support for multiple built-in chains and custom genesis configurations

## Building

Build the XLayer Reth binary from the workspace root using `just`:

```bash
# Install just if needed
cargo install just

# Standard release build
just install

# Maximum performance build (recommended for production)
just install-maxperf
```

After installation, the binary will be available as `xlayer-reth`:

```bash
xlayer-reth --help
```

## Available Commands

XLayer Reth provides three main commands:

- `xlayer-reth node` - Run a full XLayer node
- `xlayer-reth import` - Import blockchain data from RLP-encoded files
- `xlayer-reth export` - Export blockchain data to RLP-encoded files

---

# Running XLayer Node

## Overview

The `xlayer-reth node` command runs a full XLayer node with all the features needed to participate in the XLayer network.

## Features

- **Optimism Rollup Support**: Full compatibility with Optimism rollup protocol
- **Custom XLayer Extensions**: Bridge intercept, Apollo configuration, and inner transaction tracking
- **JSON-RPC API**: Complete Ethereum JSON-RPC interface with XLayer extensions
- **State Sync**: Efficient state synchronization from L1
- **ExEx Support**: Execution extensions for custom functionality

## Basic Usage

```bash
xlayer-reth node --datadir <DATA_DIR> --chain <CHAIN_SPEC> --rollup.sequencer-http <SEQUENCER_URL>
```

## Key Configuration Options

### Required Arguments

- `--datadir <DIR>`: Directory for all node data and subdirectories
- `--chain <CHAIN_SPEC>`: Chain specification (built-in name or path to genesis JSON)
- `--rollup.sequencer-http <URL>`: Sequencer HTTP endpoint for rollup

### Network Configuration

- `--http`: Enable HTTP JSON-RPC server
- `--http.addr <ADDR>`: HTTP server listening address (default: 127.0.0.1)
- `--http.port <PORT>`: HTTP server listening port (default: 8545)
- `--ws`: Enable WebSocket JSON-RPC server
- `--ws.addr <ADDR>`: WebSocket server address
- `--ws.port <PORT>`: WebSocket server port (default: 8546)

### Database Options

- `--db.max-size <SIZE>`: Maximum database size (e.g., 4TB)
- `--db.growth-step <SIZE>`: Database growth step (e.g., 4GB)
- `--db.log-level <LEVEL>`: Database logging level

### XLayer-Specific Options

- `--xlayer.enable-inner-tx`: Enable inner transaction tracking
- `--xlayer.intercept.enabled`: Enable bridge intercept functionality
- `--xlayer.apollo.enabled`: Enable Apollo configuration service

## Examples

### Example 1: Run Mainnet Node

```bash
xlayer-reth node \
    --datadir /data/xlayer-reth \
    --chain xlayer \
    --rollup.sequencer-http https://xlayerrpc.okx.com \
    --http \
    --http.addr 0.0.0.0 \
    --http.port 8545
```

### Example 2: Run Testnet Node with Inner Transaction Tracking

```bash
xlayer-reth node \
    --datadir /data/xlayer-testnet \
    --chain xlayer-testnet \
    --rollup.sequencer-http https://testrpc.xlayer.tech \
    --http \
    --ws \
    --xlayer.enable-inner-tx
```

### Example 3: Run Node with Custom Genesis

```bash
xlayer-reth node \
    --datadir /data/xlayer-custom \
    --chain /path/to/genesis.json \
    --rollup.sequencer-http http://localhost:9545 \
    --http \
    --http.addr 0.0.0.0
```

### Example 4: Archive Node Configuration

```bash
xlayer-reth node \
    --datadir /data/xlayer-archive \
    --chain xlayer \
    --rollup.sequencer-http https://xlayerrpc.okx.com \
    --http \
    --http.addr 0.0.0.0 \
    --db.max-size 10TB \
    --db.growth-step 10GB
```

## Initialization

Before running a node for the first time, initialize the database with the genesis configuration:

```bash
xlayer-reth node init --datadir <DATA_DIR> --chain <CHAIN_SPEC>
```

This command:
- Creates the database structure
- Writes the genesis block
- Initializes chain configuration
- Sets up the datadir structure

## Monitoring

The node provides several ways to monitor its operation:

### Logs

Node logs include:
- Sync progress and block imports
- RPC request handling
- XLayer feature status
- Database operations

### JSON-RPC API

Query node status via JSON-RPC:

```bash
# Get current block number
curl -X POST -H "Content-Type: application/json" \
    --data '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}' \
    http://localhost:8545

# Get sync status
curl -X POST -H "Content-Type: application/json" \
    --data '{"jsonrpc":"2.0","method":"eth_syncing","params":[],"id":1}' \
    http://localhost:8545
```

---

# Import Command

## Overview

The `xlayer-reth import` command imports blockchain data from RLP-encoded block files. This is useful for:

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

## Usage

### Basic Command

```bash
xlayer-reth import --datadir <DATA_DIR> --chain <CHAIN_SPEC> --exported-data <BLOCK_FILE>
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
xlayer-reth import \
    --datadir /data/xlayer-reth \
    --chain optimism \
    --exported-data /path/to/blocks.rlp
```

### Example 2: Import from Compressed File

Import from a gzip-compressed file:

```bash
xlayer-reth import \
    --datadir /data/xlayer-reth \
    --chain optimism \
    --exported-data /path/to/blocks.rlp.gz
```

### Example 3: Import with Custom Genesis

Import using a custom genesis file:

```bash
xlayer-reth import \
    --datadir /data/xlayer-reth \
    --chain /path/to/custom-genesis.json \
    --exported-data /path/to/blocks.rlp
```

### Example 4: Fast Import (No State Processing)

Import without state processing for faster imports (useful for testing):

```bash
xlayer-reth import \
    --datadir /data/xlayer-reth \
    --chain optimism \
    --no-state \
    --exported-data /path/to/blocks.rlp
```

### Example 5: Import with Custom Chunk Size

Import with a custom read chunk size:

```bash
xlayer-reth import \
    --datadir /data/xlayer-reth \
    --chain optimism \
    --chunk-len 1048576 \
    --exported-data /path/to/blocks.rlp
```

### Example 6: Import with Database Configuration

Import with custom database settings:

```bash
xlayer-reth import \
    --datadir /data/xlayer-reth \
    --chain optimism \
    --db.max-size 2TB \
    --db.growth-step 4GB \
    --db.log-level notice \
    --exported-data /path/to/blocks.rlp
```

## Exporting Blockchain Data

To create an RLP-encoded blocks file that can be imported, you can use `geth` or the XLayer Reth export command:

### Using XLayer Reth

```bash
# Export blocks to RLP format
xlayer-reth export \
    --datadir /data/xlayer-reth \
    --chain optimism \
    --exported-data /path/to/output.rlp

# Export specific block range
xlayer-reth export \
    --datadir /data/xlayer-reth \
    --chain optimism \
    --start-block 1000 \
    --end-block 5000 \
    --exported-data /path/to/output.rlp.gz
```

### Using Geth

```bash
# Export blocks from geth
geth export /path/to/output.rlp <start_block> <end_block>

# Export and compress
geth export /path/to/output.rlp <start_block> <end_block>
gzip /path/to/output.rlp
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


# Export Command

## Overview

The `xlayer-reth export` command exports blockchain data from your XLayer Reth node's database to RLP-encoded block files. This is useful for:

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

The export command is included in the main XLayer Reth binary. See the [Building](#building) section at the top of this document for installation instructions.

## Usage

### Basic Command

```bash
xlayer-reth export --datadir <DATA_DIR> --chain <CHAIN_SPEC> --exported-data <OUTPUT_FILE>
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
xlayer-reth export \
    --datadir /data/xlayer-reth \
    --chain optimism \
    --exported-data /backups/blocks.rlp
```

### Example 2: Export with Compression

Export all blocks to a compressed file:

```bash
xlayer-reth export \
    --datadir /data/xlayer-reth \
    --chain optimism \
    --exported-data /backups/blocks.rlp.gz
```

### Example 3: Export Specific Block Range

Export blocks from 1000 to 5000:

```bash
xlayer-reth export \
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
xlayer-reth export \
    --datadir /data/xlayer-reth \
    --chain optimism \
    --start-block $START_BLOCK \
    --exported-data /backups/recent-blocks.rlp.gz
```

### Example 5: Export with Custom Batch Size

Export with a larger batch size for better performance:

```bash
xlayer-reth export \
    --datadir /data/xlayer-reth \
    --chain optimism \
    --batch-size 5000 \
    --exported-data /backups/blocks.rlp.gz
```

### Example 6: Export Using Custom Genesis

Export using a custom genesis file:

```bash
xlayer-reth export \
    --datadir /data/xlayer-reth \
    --chain /path/to/custom-genesis.json \
    --exported-data /backups/blocks.rlp.gz
```

### Example 7: Export to Network Storage

Export directly to a network-mounted backup directory:

```bash
xlayer-reth export \
    --datadir /data/xlayer-reth \
    --chain optimism \
    --exported-data /mnt/network-backup/xlayer-blocks-$(date +%Y%m%d).rlp.gz
```

## Importing Exported Data

Once you've exported blockchain data, you can import it into another node using the `xlayer-reth import` command:

```bash
xlayer-reth import \
    --datadir /data/new-node \
    --chain optimism \
    --exported-data /backups/blocks.rlp.gz
```

See the [Import Command](#import-command) section for more details.

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
xlayer-reth export \
    --datadir /data/old-node \
    --chain optimism \
    --exported-data /transfer/blocks.rlp.gz

# On new node
xlayer-reth import \
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

xlayer-reth export \
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
xlayer-reth export \
    --datadir /data/prod-node \
    --chain optimism \
    --start-block 1000000 \
    --end-block 1001000 \
    --exported-data /test-data/recent-blocks.rlp

# Import to test node
xlayer-reth import \
    --datadir /data/test-node \
    --chain optimism \
    --exported-data /test-data/recent-blocks.rlp
```

### 4. Data Analysis

Export specific block ranges for analysis:

```bash
xlayer-reth export \
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

