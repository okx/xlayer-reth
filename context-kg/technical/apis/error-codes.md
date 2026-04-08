# Error Codes and Handling

## JSON-RPC Standard Error Codes

| Code | Constant | Usage |
|------|----------|-------|
| `-32700` | `PARSE_ERROR_CODE` | Invalid JSON from legacy endpoint |
| `-32600` | `INVALID_REQUEST_CODE` | Malformed request |
| `-32603` | `INTERNAL_ERROR_CODE` | Legacy RPC connection failure, response parse error, invalid legacy response |
| `-32015` | `CALL_EXECUTION_FAILED_CODE` | Default for legacy RPC error responses without explicit code |

## Bridge Intercept Errors

These are NOT RPC errors — they are internal builder errors that cause transaction exclusion:

| Error | Context |
|-------|---------|
| `BridgeInterceptError::WildcardBlock` | Wildcard mode: any BridgeEvent from bridge contract |
| `BridgeInterceptError::TargetTokenBlock` | Target token mode: matching originAddress |

These are logged at `debug` level and do not propagate to RPC users.

## Configuration Validation Errors

Returned as `String` from `validate()` methods, causing process exit:

| Error Message | Source |
|---------------|--------|
| `"Invalid legacy RPC URL '{url}': {e}"` | `LegacyRpcArgs::validate()` |
| `"Legacy RPC timeout must be greater than zero"` | `LegacyRpcArgs::validate()` |
| `"--xlayer.intercept.bridge-contract is required when interception is enabled"` | `XLayerInterceptArgs::validate()` |
| `"Invalid bridge contract address: {addr}"` | `XLayerInterceptArgs::validate()` |
| `"Invalid target token address: {token}"` | `XLayerInterceptArgs::validate()` |

## Legacy RPC Error Wrapping

When forwarding errors from the legacy endpoint:

```rust
// Legacy returns JSON-RPC error → extract code and message, forward as-is
ErrorObject::owned(code, message, None::<()>)

// Legacy endpoint unreachable → wrap in INTERNAL_ERROR
ErrorObject::owned(INTERNAL_ERROR_CODE, format!("Legacy RPC error: {e}"), None::<()>)

// Legacy response unparseable → wrap in INTERNAL_ERROR
ErrorObject::owned(INTERNAL_ERROR_CODE, format!("Legacy parse error: {e}"), None::<()>)

// Legacy response has neither result nor error
ErrorObject::owned(INTERNAL_ERROR_CODE, "Invalid legacy response", None::<()>)
```

## Input Validation Errors

| Validation | Error Behavior |
|------------|----------------|
| Invalid block hash format | `call_eth_get_block_by_hash` returns `Err("Invalid block hash format: {hash}")` |
| Non-hex characters in hash | `parse_block_param` returns `None` (silently treated as non-routable) |
| Invalid JSON params | `RawValue::from_string` returns `Err` → forwarded as RPC error or returns `None` |
