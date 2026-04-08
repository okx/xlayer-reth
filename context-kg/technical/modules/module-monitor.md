# Module: xlayer_monitor

**Path**: `crates/monitor/`
**Purpose**: Full-link transaction lifecycle monitoring for observability

## Components

### Configuration (`args.rs`)
- `FullLinkMonitorArgs`: `enable` flag + `output_path` (default: `/data/logs/trace.log`)
- Validation: checks that output path parent directory exists

### Core Monitor (`monitor.rs`)
`XLayerMonitor` — Records lifecycle events:

**Sequencer Mode Events**:
- `on_recv_transaction()` → `SeqReceiveTxEnd`
- `on_block_build_start()` → `SeqBlockBuildStart`
- `on_tx_commit()` → `SeqTxExecutionEnd`
- `on_block_commit()` → `SeqBlockBuildEnd`
- `on_block_send_start()` → `SeqBlockSendStart`

**RPC Mode Events**:
- `on_recv_transaction()` → `RpcReceiveTxEnd`
- `on_block_received()` → `RpcBlockReceiveEnd`
- `on_block_commit()` → `RpcBlockInsertEnd`

### Event Handle (`handle.rs`)
`start_monitor_handle()` — Subscribes to:
- Consensus engine events: `BlockReceived`, `CanonicalBlockAdded`
- Payload builder events: `Attributes`, `BuiltPayload`
- Runs as a critical task (node shuts down if it panics)

### RPC Middleware (`rpc.rs`)
- `RpcMonitorLayer` / `RpcMonitorService<S>`: Tower middleware
- Intercepts `eth_sendRawTransaction` and `eth_sendTransaction`
- Extracts transaction hash from successful RPC responses
- Calls `monitor.on_recv_transaction()` with the extracted hash

## Integration with xlayer_trace_monitor

Events are recorded via the external `xlayer_trace_monitor` crate which writes structured trace data to the configured output path. The monitor uses a global tracer initialized in `main.rs` when `--xlayer.monitor.enable` is set.

## Key Design Points

1. **Fire-and-forget**: Monitor failures must never affect node operation (see `knowledge-base.md` Rule 6).
2. **Mode-aware**: Event names differ based on sequencer vs RPC mode, enabling per-mode dashboards.
3. **Middleware position**: `RpcMonitorLayer` executes BEFORE `LegacyRpcRouterLayer` in the middleware stack.
