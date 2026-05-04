# xlayer-trace-monitor

Transaction and block lifecycle tracing crate for X Layer.

## Overview

A lightweight crate for logging transaction and block lifecycle events to CSV files. Uses buffered writes for performance and supports global singleton pattern.

## Features

- 8 monitoring points covering transaction/block lifecycle
- CSV output with 23 fields
- Buffered writes with auto-flush (100 writes or 1s)
- Zero overhead when disabled
- Default path: `/data/logs/trace.log`

## Usage

### Add Dependency

```toml
[dependencies]
xlayer-trace-monitor = { git = "https://github.com/okx/xlayer-toolkit", path = "crates/xlayer-trace-monitor" }
```

### Initialize

```rust
use xlayer_trace_monitor::init_global_tracer;
use std::path::PathBuf;

// Initialize at startup
init_global_tracer(
    true,  // enabled
    Some(PathBuf::from("/path/to/trace.log")),  // optional, defaults to /data/logs/trace.log
);
```

### Use in Code

```rust
use xlayer_trace_monitor::{get_global_tracer, TransactionProcessId, Hash32};

// Log transaction event
if let Some(tracer) = get_global_tracer() {
    let tx_hash: Hash32 = [0x12; 32];  // or convert from B256: *b256_hash.as_ref()
    tracer.log_transaction(tx_hash, TransactionProcessId::SeqReceiveTxEnd, Some(12345));
}

// Log block event
if let Some(tracer) = get_global_tracer() {
    let block_hash: Hash32 = [0x34; 32];
    tracer.log_block(block_hash, 12345, TransactionProcessId::SeqBlockBuildEnd);
}
```

## Monitoring Points

| ID | Enum | Description |
|----|------|-------------|
| 15010 | `RpcReceiveTxEnd` | RPC node received transaction |
| 15030 | `SeqReceiveTxEnd` | Sequencer node received transaction |
| 15032 | `SeqBlockBuildStart` | Sequencer started building block |
| 15034 | `SeqTxExecutionEnd` | Sequencer completed transaction execution |
| 15036 | `SeqBlockBuildEnd` | Sequencer completed building block |
| 15042 | `SeqBlockSendStart` | Sequencer started sending block |
| 15060 | `RpcBlockReceiveEnd` | RPC node received block |
| 15062 | `RpcBlockInsertEnd` | RPC node completed block insertion |

## API

### Functions

- `init_global_tracer(enabled, output_path)` - Initialize singleton tracer
- `get_global_tracer()` - Get tracer instance
- `flush_global_tracer()` - Force flush
- `sync_global_tracer()` - Force sync to disk

### Types

- `Hash32` - Type alias for `[u8; 32]` (32-byte hash)
- `TransactionTracer` - Main tracer struct
- `TransactionProcessId` - Enum for monitoring point IDs

### Methods

- `tracer.log_transaction(hash, process_id, block_number)` - Log transaction
- `tracer.log_block(hash, block_number, process_id)` - Log block
- `tracer.log_block_with_timestamp(hash, block_number, process_id, timestamp_ms)` - Log with timestamp
- `tracer.flush()` - Flush buffer
- `tracer.sync_all()` - Sync to disk

## Notes

- Tracer must be initialized before use
- Auto-flush: every 100 writes or 1 second
- Files opened in append mode
- Zero overhead when disabled

## License

MIT OR Apache-2.0
