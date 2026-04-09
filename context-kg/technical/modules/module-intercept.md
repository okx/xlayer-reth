# Module: xlayer_bridge_intercept

**Path**: `crates/intercept/`
**Purpose**: Selective blocking of PolygonZkEVMBridge transactions during payload building

## Components

### Configuration
- `BridgeInterceptConfig`: Runtime config with `enabled`, `bridge_contract_address`, `target_token_address`, `wildcard` flag
- Default: disabled (all fields zeroed)

### Core Function
- `intercept_bridge_transaction_if_need(logs, tx_sender, config) -> Result<(), BridgeInterceptError>`
  - Scans execution logs for `BridgeEvent` from the configured bridge contract
  - **Wildcard mode** (`wildcard: true`): Any `BridgeEvent` from the bridge contract triggers interception
  - **Target token mode** (`wildcard: false`): Only intercepts if `originAddress` matches `target_token_address`
  - Returns `Ok(())` to allow, `Err(BridgeInterceptError)` to block

### Event Parsing
- `BRIDGE_EVENT_SIGNATURE`: keccak256 of `BridgeEvent(uint8,uint32,address,uint32,address,uint256,bytes,uint32)`
- `parse_bridge_event(log)`: Extracts `originAddress` (data[76..96]) and `amount` (data[160..192]) from ABI-encoded log data
- Requires minimum 256 bytes of log data

### Error Types
- `BridgeInterceptError::WildcardBlock { bridge_contract, sender }` — Wildcard mode interception
- `BridgeInterceptError::TargetTokenBlock { token, amount, sender }` — Specific token interception

## CLI Arguments (defined in `bin/node/src/args.rs`)
- `--xlayer.intercept.enabled` (default: false)
- `--xlayer.intercept.bridge-contract` (required when enabled)
- `--xlayer.intercept.target-token` (optional; omission or `*` = wildcard mode)

## Key Design Points

1. **Post-execution check**: Interception happens AFTER EVM execution by inspecting logs, not before. This means the transaction is fully executed and then discarded if intercepted.
2. **Payload-builder only**: Does NOT affect transaction pool, RPC forwarding, or block validation.
3. **Validation in args layer**: Address parsing and format validation happen in `XLayerInterceptArgs::validate()` before any runtime use.
4. **Disabled path skips parsing**: When `enabled: false`, `to_bridge_intercept_config()` returns default config without parsing any addresses — preventing panics from invalid address strings in disabled configs.
